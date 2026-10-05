use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use uefi::boot::{self, image_handle};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
use uefi::{Result, Status, cstr16};

#[derive(Default)]
pub struct Config {
    pub version: Option<String>,
    pub rollback: Option<Rollback>,
    pub rotations: Vec<(String, u8)>,
}

pub struct Rollback {
    pub entry: String,
    pub version: String,
    pub dtbs: String,
}

pub fn load() -> Result<Config> {
    let mut file_system = boot::get_image_file_system(image_handle())?;
    let mut root = file_system.open_volume()?;
    let mut file = root
        .open(
            cstr16!(r"\armada\backend.conf"),
            FileMode::Read,
            FileAttribute::READ_ONLY,
        )?
        .into_regular_file()
        .ok_or(Status::UNSUPPORTED)?;
    let size = usize::try_from(file.get_boxed_info::<FileInfo>()?.file_size())
        .map_err(|_| Status::BAD_BUFFER_SIZE)?;
    let mut data = vec![0; size];
    let read = file.read(&mut data)?;
    let text = str::from_utf8(&data[..read]).map_err(|_| Status::LOAD_ERROR)?;
    let value = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };

    let rollback = match (
        value("ARMADA_ROLLBACK_ENTRY"),
        value("ARMADA_ROLLBACK_VERSION"),
        value("ARMADA_ROLLBACK_DTBS"),
    ) {
        (Some(entry), Some(version), Some(dtbs)) => Some(Rollback {
            entry: entry.to_string(),
            version: version.to_string(),
            dtbs: dtbs.to_string(),
        }),
        _ => None,
    };

    Ok(Config {
        version: value("ARMADA_DEFAULT_VERSION").map(ToString::to_string),
        rollback,
        rotations: text
            .lines()
            .filter_map(|line| {
                let (model, degrees) = line
                    .strip_prefix("ARMADA_EFI_ROTATION=")?
                    .rsplit_once(':')?;
                let turns = match degrees {
                    "0" => 0,
                    "90" => 1,
                    "180" => 2,
                    "270" => 3,
                    _ => return None,
                };
                Some((model.to_string(), turns))
            })
            .collect(),
    })
}
