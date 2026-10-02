#![no_main]
#![no_std]

extern crate alloc;

mod config;
mod dtb;
mod ui;

use alloc::vec::Vec;
use core::{str, time::Duration};
use uefi::boot::{self, LoadImageSource, image_handle};
use uefi::prelude::*;
use uefi::proto::BootPolicy;
use uefi::proto::device_path::{DevicePath, build};
use uefi::proto::loaded_image::LoadedImage;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{CStr16, CString16, Result, Status, cstr16, guid};

use ui::Choice;

const DTB_LOADER: &CStr16 = cstr16!(r"\EFI\BOOT\drivers_aa64\adtbloaderaa64.efi");
const SYSTEMD_BOOT: &CStr16 = cstr16!(r"\EFI\systemd\systemd-bootaa64.efi");
const ARMADA_DEVICE: &CStr16 = cstr16!("ArmadaDevice");
const ARMADA: VariableVendor = VariableVendor(guid!("a2ba216f-a695-49fb-aa72-9f57de4ab768"));
const SYSTEMD: VariableVendor = VariableVendor(guid!("4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"));

fn load(path: &CStr16) -> Result<Handle> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(image_handle())?;
    let device = image.device().ok_or(Status::NOT_FOUND)?;
    drop(image);

    let device_path = boot::open_protocol_exclusive::<DevicePath>(device)?;
    let mut buffer = Vec::new();
    let mut builder = build::DevicePathBuilder::with_vec(&mut buffer);
    for node in device_path.node_iter() {
        builder = builder.push(&node).map_err(|_| Status::OUT_OF_RESOURCES)?;
    }
    let path = builder
        .push(&build::media::FilePath { path_name: path })
        .and_then(|builder| builder.finalize())
        .map_err(|_| Status::OUT_OF_RESOURCES)?;

    boot::load_image(
        image_handle(),
        LoadImageSource::FromDevicePath {
            device_path: path,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
}

fn select(entry: &str) -> Result {
    let entry = if entry.ends_with(".conf") {
        CString16::try_from(entry)
    } else {
        CString16::try_from(alloc::format!("{entry}.conf").as_str())
    }
    .map_err(|_| Status::INVALID_PARAMETER)?;
    runtime::set_variable(
        cstr16!("LoaderEntryOneShot"),
        &SYSTEMD,
        VariableAttributes::NON_VOLATILE
            | VariableAttributes::BOOTSERVICE_ACCESS
            | VariableAttributes::RUNTIME_ACCESS,
        entry.as_bytes(),
    )
}

fn persisted_device(trees: &[dtb::DeviceTree]) -> Option<usize> {
    let (data, _) = runtime::get_variable_boxed(ARMADA_DEVICE, &ARMADA).ok()?;
    let name = str::from_utf8(&data).ok()?;
    trees.iter().position(|tree| tree.name == name)
}

fn choose_device(
    trees: &[dtb::DeviceTree],
    current: Option<usize>,
    cancellable: bool,
) -> Result<Option<usize>> {
    let models: Vec<_> = trees.iter().map(|tree| tree.model.as_str()).collect();
    let current = current.map(|index| trees[index].model.as_str());
    let Some(selected) = ui::device_menu(&models, current, cancellable) else {
        return Ok(None);
    };
    dtb::install(&trees[selected])?;
    runtime::set_variable(
        ARMADA_DEVICE,
        &ARMADA,
        VariableAttributes::NON_VOLATILE
            | VariableAttributes::BOOTSERVICE_ACCESS
            | VariableAttributes::RUNTIME_ACCESS,
        trees[selected].name.as_bytes(),
    )?;
    Ok(Some(selected))
}

fn run() -> Result {
    if let Ok(driver) = load(DTB_LOADER) {
        let _ = boot::start_image(driver);
    }

    let config = config::load().unwrap_or_default();
    let trees = dtb::available().unwrap_or_default();
    if trees.is_empty() {
        return Err(Status::NOT_FOUND.into());
    }
    let mut device = persisted_device(&trees);
    if let Some(index) = device {
        dtb::install(&trees[index])?;
    } else {
        device = dtb::detected(&trees);
    }
    while device.is_none() {
        device = choose_device(&trees, device, false)?;
    }
    let mut timed = true;
    loop {
        let rollback = config.rollback.as_ref().and_then(|rollback| {
            let tree = dtb::from(&rollback.dtbs, &trees.get(device?)?.name).ok()?;
            Some((rollback, tree))
        });
        match ui::menu(
            config.version.as_deref(),
            rollback
                .as_ref()
                .map(|(rollback, _)| rollback.version.as_str()),
            device.map(|index| trees[index].model.as_str()),
            timed,
        ) {
            Choice::Armada => break,
            Choice::Previous => {
                let (rollback, tree) = rollback.ok_or(Status::NOT_FOUND)?;
                dtb::install(&tree)?;
                select(&rollback.entry)?;
                break;
            }
            Choice::Device => {
                if let Some(selected) = choose_device(&trees, device, true)? {
                    device = Some(selected);
                }
                timed = false;
            }
        }
    }

    ui::clear();
    boot::start_image(load(SYSTEMD_BOOT)?)
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    match run() {
        Ok(()) => Status::SUCCESS,
        Err(error) => {
            uefi::println!("Armada Boot failed: {:?}", error.status());
            boot::stall(Duration::from_secs(5));
            error.status()
        }
    }
}
