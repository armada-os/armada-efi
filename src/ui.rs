use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::fmt::Write;
use core::time::Duration;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::image::Image;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::primitives::{Primitive, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::{Drawable, Pixel};
use fdt::Fdt;
use tinybmp::Bmp;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{FontRenderer, fonts};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, image_handle};
use uefi::proto::ProtocolPointer;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::console::text::{Color, Key, ScanCode};
use uefi::{Handle, Result, guid, system};

const LOGO: &[u8] = include_bytes!("../assets/armada.bmp");
const DEVICE_TREE: uefi::Guid = guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");

const WHITE: Rgb888 = Rgb888::WHITE;
const MUTED: Rgb888 = Rgb888::new(0x9a, 0x9a, 0xa0);
const SELECTED: Rgb888 = Rgb888::new(0x28, 0x28, 0x2d);

#[derive(Clone, Copy)]
pub enum Choice {
    Armada,
    Previous,
    Device,
}

#[derive(Clone, Copy)]
struct Item<'a> {
    label: &'a str,
    detail: Option<&'a str>,
}

impl<'a> Item<'a> {
    fn new(label: &'a str) -> Self {
        Self {
            label,
            detail: None,
        }
    }
}

struct Canvas {
    pixels: Vec<BltPixel>,
    width: usize,
    height: usize,
    turns: u8,
}

impl Canvas {
    fn new(width: usize, height: usize, turns: u8) -> Self {
        Self {
            pixels: vec![BltPixel::new(0, 0, 0); width * height],
            width,
            height,
            turns: turns % 4,
        }
    }

    fn show(&self, output: &mut GraphicsOutput) -> Result {
        output.blt(BltOp::BufferToVideo {
            buffer: &self.pixels,
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (self.width, self.height),
        })
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        let size = Size::new(self.width as u32, self.height as u32);
        if self.turns % 2 == 1 {
            Size::new(size.height, size.width)
        } else {
            size
        }
    }
}

impl DrawTarget for Canvas {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> core::result::Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let size = self.size();
        for Pixel(point, color) in pixels {
            let (Ok(x), Ok(y)) = (usize::try_from(point.x), usize::try_from(point.y)) else {
                continue;
            };
            if x >= size.width as usize || y >= size.height as usize {
                continue;
            }
            let (x, y) = match self.turns {
                1 => (self.width - 1 - y, x),
                2 => (self.width - 1 - x, self.height - 1 - y),
                3 => (y, self.height - 1 - x),
                _ => (x, y),
            };
            self.pixels[y * self.width + x] = BltPixel::new(color.r(), color.g(), color.b());
        }
        Ok(())
    }
}

fn open<P: ProtocolPointer + ?Sized>(handle: Handle) -> Result<boot::ScopedProtocol<P>> {
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams {
                handle,
                agent: image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

fn rotation(width: usize, height: usize) -> u8 {
    device_tree_rotation().unwrap_or_else(|| u8::from(height > width))
}

fn device_tree_rotation() -> Option<u8> {
    system::with_config_table(|tables| {
        let table = tables.iter().find(|table| table.guid == DEVICE_TREE)?;
        let tree = unsafe { Fdt::from_ptr(table.address.cast()) }.ok()?;
        let degrees = tree
            .all_nodes()
            .filter(|node| node.name.split('@').next() == Some("panel"))
            .find_map(|node| node.property("rotation")?.as_usize())?;
        match degrees {
            0 => Some(0),
            90 => Some(1),
            180 => Some(2),
            270 => Some(3),
            _ => None,
        }
    })
}

fn draw_graphics(items: &[Item], selected: usize, countdown: Option<u8>) -> Option<()> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut output = open::<GraphicsOutput>(handle).ok()?;
    let (width, height) = output.current_mode_info().resolution();
    let mut canvas = Canvas::new(width, height, rotation(width, height));
    let size = canvas.size();
    let center = size.width as i32 / 2;

    system::with_stdout(|stdout| {
        let _ = stdout.enable_cursor(false);
    });

    let logo = Bmp::<Rgb888>::from_slice(LOGO).ok()?;
    let logo_x = center - logo.size().width as i32 / 2;
    let logo_y = size.height as i32 / 9;
    Image::new(&logo, Point::new(logo_x, logo_y))
        .draw(&mut canvas)
        .ok()?;

    let item = FontRenderer::new::<fonts::u8g2_font_fub30_tr>();
    let hint = FontRenderer::new::<fonts::u8g2_font_fur17_tr>();

    let row_height = if items.iter().any(|item| item.detail.is_some()) {
        92
    } else {
        76
    };
    let mut y = logo_y + logo.size().height as i32 + 70;
    let bar = Size::new((size.width * 2 / 3).min(760), row_height as u32);
    let rows = ((size.height as i32 - y - 170) / (row_height + 12)).max(1) as usize;
    let first = selected
        .saturating_sub(rows / 2)
        .min(items.len().saturating_sub(rows));
    for (index, entry) in items.iter().enumerate().skip(first).take(rows) {
        let color = if index == selected { WHITE } else { MUTED };
        if index == selected {
            let area = Rectangle::new(Point::new(center - bar.width as i32 / 2, y), bar);
            RoundedRectangle::with_equal_corners(area, Size::new(14, 14))
                .into_styled(PrimitiveStyle::with_fill(SELECTED))
                .draw(&mut canvas)
                .ok()?;
        }
        item.render_aligned(
            entry.label,
            Point::new(
                center,
                y + if entry.detail.is_some() {
                    30
                } else {
                    row_height / 2
                },
            ),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(color),
            &mut canvas,
        )
        .ok()?;
        if let Some(detail) = entry.detail {
            hint.render_aligned(
                detail,
                Point::new(center, y + 67),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(MUTED),
                &mut canvas,
            )
            .ok()?;
        }
        y += row_height + 12;
    }

    let countdown = match countdown {
        Some(3) => Some("Booting in 3"),
        Some(2) => Some("Booting in 2"),
        Some(1) => Some("Booting in 1"),
        _ => None,
    };
    if let Some(countdown) = countdown {
        hint.render_aligned(
            countdown,
            Point::new(center, size.height as i32 - 125),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(MUTED),
            &mut canvas,
        )
        .ok()?;
    }

    hint.render_aligned(
        "VOL+/VOL- or arrows - POWER or Enter to select",
        Point::new(center, size.height as i32 - 70),
        VerticalPosition::Center,
        HorizontalAlignment::Center,
        FontColor::Transparent(MUTED),
        &mut canvas,
    )
    .ok()?;
    canvas.show(&mut output).ok()
}

fn draw_text(items: &[Item], selected: usize, countdown: Option<u8>) {
    system::with_stdout(|stdout| {
        let _ = stdout.clear();
        let _ = stdout.enable_cursor(false);
        for (index, item) in items.iter().enumerate() {
            let _ = writeln!(
                stdout,
                "{} {}\r",
                if selected == index { ">" } else { " " },
                item.label,
            );
            if let Some(detail) = item.detail {
                let _ = writeln!(stdout, "  {detail}\r",);
            }
        }
        if let Some(countdown) = countdown {
            let _ = writeln!(stdout, "\r\nBooting in {countdown}\r");
        }
        let _ = writeln!(stdout, "\r\nUse arrows and Enter to select\r");
    });
}

fn draw(items: &[Item], selected: usize, countdown: Option<u8>) {
    if draw_graphics(items, selected, countdown).is_none() {
        draw_text(items, selected, countdown);
    }
}

fn wait_for_release() {
    let mut idle = 0;
    for _ in 0..50 {
        let pressed = system::with_stdin(|stdin| stdin.read_key().ok().flatten().is_some());
        idle = if pressed { 0 } else { idle + 1 };
        if idle == 3 {
            return;
        }
        boot::stall(Duration::from_millis(100));
    }
}

fn choose(items: &[Item], timed: bool) -> usize {
    let mut selected = 0;
    system::with_stdin(|stdin| {
        let _ = stdin.reset(false);
    });
    draw(items, selected, timed.then_some(3));

    let mut timeout = timed.then_some(60u8);
    loop {
        let key = system::with_stdin(|stdin| stdin.read_key().ok().flatten());
        match key {
            Some(Key::Special(ScanCode::UP)) => {
                timeout = None;
                selected = selected.checked_sub(1).unwrap_or(items.len() - 1);
                draw(items, selected, None);
                boot::stall(Duration::from_millis(250));
            }
            Some(Key::Special(ScanCode::DOWN)) => {
                timeout = None;
                selected = (selected + 1) % items.len();
                draw(items, selected, None);
                boot::stall(Duration::from_millis(250));
            }
            Some(Key::Special(ScanCode::SUSPEND)) => break,
            Some(Key::Printable(key)) if key == '\r' => break,
            _ => {
                if let Some(polls) = timeout.as_mut() {
                    *polls -= 1;
                    if *polls == 0 {
                        break;
                    }
                    if *polls % 20 == 0 {
                        draw(items, selected, Some(*polls / 20));
                    }
                }
                boot::stall(Duration::from_millis(50));
            }
        }
    }

    wait_for_release();
    selected
}

pub fn menu(
    version: Option<&str>,
    rollback: Option<&str>,
    device: Option<&str>,
    timed: bool,
) -> Choice {
    let device_detail = device.map(|device| alloc::format!("Current: {device}"));
    let mut items = vec![Item {
        label: "ArmadaOS",
        detail: version,
    }];
    if let Some(version) = rollback {
        items.push(Item {
            label: "Rollback Version",
            detail: Some(version),
        });
    }
    items.push(Item {
        label: "Device Override",
        detail: device_detail.as_deref(),
    });
    match choose(&items, timed) {
        0 => Choice::Armada,
        1 if rollback.is_some() => Choice::Previous,
        _ => Choice::Device,
    }
}

pub fn device_menu(models: &[&str]) -> Option<usize> {
    let mut manufacturers = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let manufacturer = model.split_once(' ').map_or(*model, |(name, _)| name);
        if manufacturers.last().map(|(name, _)| *name) != Some(manufacturer) {
            manufacturers.push((manufacturer, index));
        }
    }

    loop {
        let mut items: Vec<_> = manufacturers
            .iter()
            .map(|(name, _)| Item::new(name))
            .collect();
        items.push(Item::new("Back"));
        let manufacturer = choose(&items, false);
        if manufacturer == manufacturers.len() {
            return None;
        }

        let start = manufacturers[manufacturer].1;
        let end = manufacturers
            .get(manufacturer + 1)
            .map_or(models.len(), |(_, index)| *index);
        let mut items: Vec<_> = models[start..end]
            .iter()
            .map(|model| Item::new(model.split_once(' ').map_or(*model, |(_, name)| name)))
            .collect();
        items.push(Item::new("Back"));
        let selected = choose(&items, false);
        if selected < end - start {
            return Some(start + selected);
        }
    }
}

pub fn clear() {
    if let Ok(handle) = boot::get_handle_for_protocol::<GraphicsOutput>()
        && let Ok(mut output) = open::<GraphicsOutput>(handle)
    {
        let size = output.current_mode_info().resolution();
        let _ = output.blt(BltOp::VideoFill {
            color: BltPixel::new(0, 0, 0),
            dest: (0, 0),
            dims: size,
        });
    }
    system::with_stdout(|stdout| {
        let _ = stdout.set_color(Color::LightGray, Color::Black);
        let _ = stdout.clear();
        let _ = stdout.enable_cursor(true);
    });
}
