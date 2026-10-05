use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::fmt::Write;
use core::time::Duration;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::image::Image;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::primitives::{Line, Primitive, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::{Drawable, Pixel};
use fdt::Fdt;
use tinybmp::Bmp;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{FontRenderer, fonts};
use uefi::boot::{
    self, EventType, OpenProtocolAttributes, OpenProtocolParams, TimerTrigger, Tpl, image_handle,
};
use uefi::proto::ProtocolPointer;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::console::text::{Color, Key, ScanCode};
use uefi::{Handle, Result, guid, system};

use crate::config::Config;

const LOGO: &[u8] = include_bytes!("../assets/armada.bmp");
const DEVICE_TREE: uefi::Guid = guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");

const WHITE: Rgb888 = Rgb888::WHITE;
const MUTED: Rgb888 = Rgb888::new(0x9a, 0x9a, 0xa0);
const CARD: Rgb888 = Rgb888::new(0x18, 0x18, 0x1a);

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
    back: bool,
}

impl<'a> Item<'a> {
    fn new(label: &'a str) -> Self {
        Self {
            label,
            detail: None,
            back: false,
        }
    }

    fn back() -> Self {
        Self {
            label: "Back",
            detail: None,
            back: true,
        }
    }
}

#[derive(Clone, Copy)]
struct Page<'a> {
    config: &'a Config,
    title: &'a str,
    device: Option<&'a str>,
    confirm_hint: bool,
}

struct Canvas {
    pixels: Vec<BltPixel>,
    width: usize,
    height: usize,
    turns: u8,
    scale: usize,
}

impl Canvas {
    fn new(width: usize, height: usize, turns: u8) -> Self {
        let scale = (height.min(width) / 900).max(1);
        Self {
            pixels: vec![BltPixel::new(0, 0, 0); width * height],
            width,
            height,
            turns: turns % 4,
            scale,
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
        let size = Size::new(
            (self.width / self.scale) as u32,
            (self.height / self.scale) as u32,
        );
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
            let (rx, ry) = match self.turns {
                1 => (self.width / self.scale - 1 - y, x),
                2 => (
                    self.width / self.scale - 1 - x,
                    self.height / self.scale - 1 - y,
                ),
                3 => (y, self.height / self.scale - 1 - x),
                _ => (x, y),
            };
            let pixel = BltPixel::new(color.r(), color.g(), color.b());
            for dy in 0..self.scale {
                for dx in 0..self.scale {
                    let px = rx * self.scale + dx;
                    let py = ry * self.scale + dy;
                    self.pixels[py * self.width + px] = pixel;
                }
            }
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

fn rotation(width: usize, height: usize, config: &Config) -> u8 {
    device_tree_rotation(config).unwrap_or_else(|| u8::from(height > width))
}

fn device_tree_rotation(config: &Config) -> Option<u8> {
    system::with_config_table(|tables| {
        let table = tables.iter().find(|table| table.guid == DEVICE_TREE)?;
        let tree = unsafe { Fdt::from_ptr(table.address.cast()) }.ok()?;
        if let Some((_, turns)) = config
            .rotations
            .iter()
            .find(|(model, _)| model == tree.root().model())
        {
            return Some(*turns);
        }
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

fn render_pair(
    canvas: &mut Canvas,
    bold: &FontRenderer,
    regular: &FontRenderer,
    center: i32,
    y: i32,
    label: &str,
    value: &str,
) -> Option<()> {
    let label_width = bold
        .get_rendered_dimensions(label, Point::zero(), VerticalPosition::Center)
        .ok()?
        .advance
        .x;
    let value_width = regular
        .get_rendered_dimensions(value, Point::zero(), VerticalPosition::Center)
        .ok()?
        .advance
        .x;
    let x = center - (label_width + value_width) / 2;
    bold.render(
        label,
        Point::new(x, y),
        VerticalPosition::Center,
        FontColor::Transparent(MUTED),
        canvas,
    )
    .ok()?;
    regular
        .render(
            value,
            Point::new(x + label_width, y),
            VerticalPosition::Center,
            FontColor::Transparent(MUTED),
            canvas,
        )
        .ok()?;
    Some(())
}

fn draw_graphics(page: Page, items: &[Item], selected: usize, countdown: Option<u8>) -> Option<()> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut output = open::<GraphicsOutput>(handle).ok()?;
    let (width, height) = output.current_mode_info().resolution();
    let mut canvas = Canvas::new(width, height, rotation(width, height, page.config));
    let size = canvas.size();
    let left_center = size.width as i32 / 4;
    let right_center = size.width as i32 * 3 / 4;

    system::with_stdout(|stdout| {
        let _ = stdout.enable_cursor(false);
    });

    let logo = Bmp::<Rgb888>::from_slice(LOGO).ok()?;
    let info_box_height = 240;
    let gap = 54;
    let total_left_height = logo.size().height as i32
        + if page.device.is_some() {
            info_box_height + gap
        } else {
            0
        };
    let logo_x = left_center - logo.size().width as i32 / 2;
    let logo_y = (size.height as i32 - total_left_height) / 2;
    Image::new(&logo, Point::new(logo_x, logo_y))
        .draw(&mut canvas)
        .ok()?;

    let item = FontRenderer::new::<fonts::u8g2_font_fub35_tr>();
    let hint = FontRenderer::new::<fonts::u8g2_font_fur20_tr>();
    let hint_bold = FontRenderer::new::<fonts::u8g2_font_fub20_tr>();
    let title = FontRenderer::new::<fonts::u8g2_font_fub42_tr>();
    let info_label = FontRenderer::new::<fonts::u8g2_font_fub17_tr>();
    let info_val = FontRenderer::new::<fonts::u8g2_font_fub25_tr>();

    if let Some(device) = page.device {
        let box_width = (logo.size().width as i32).max(380);
        let box_x = left_center - box_width / 2;
        let box_y = logo_y + logo.size().height as i32 + gap;
        let area = Rectangle::new(
            Point::new(box_x, box_y),
            Size::new(box_width as u32, info_box_height as u32),
        );
        RoundedRectangle::with_equal_corners(area, Size::new(14, 14))
            .into_styled(PrimitiveStyle::with_fill(CARD))
            .draw(&mut canvas)
            .ok()?;

        let pad_x = box_x + 24;
        info_label
            .render(
                "INFO",
                Point::new(pad_x, box_y + 32),
                VerticalPosition::Center,
                FontColor::Transparent(MUTED),
                &mut canvas,
            )
            .ok()?;
        info_label
            .render(
                "SELECTED DEVICE",
                Point::new(pad_x, box_y + 76),
                VerticalPosition::Center,
                FontColor::Transparent(MUTED),
                &mut canvas,
            )
            .ok()?;
        info_val
            .render(
                device,
                Point::new(pad_x, box_y + 110),
                VerticalPosition::Center,
                FontColor::Transparent(WHITE),
                &mut canvas,
            )
            .ok()?;
        info_label
            .render(
                "EFI BOOTLOADER VERSION",
                Point::new(pad_x, box_y + 160),
                VerticalPosition::Center,
                FontColor::Transparent(MUTED),
                &mut canvas,
            )
            .ok()?;
        info_val
            .render(
                env!("ARMADA_BOOT_VERSION"),
                Point::new(pad_x, box_y + 196),
                VerticalPosition::Center,
                FontColor::Transparent(WHITE),
                &mut canvas,
            )
            .ok()?;
    }

    let title_y = (size.height as i32 / 8).max(90);
    title
        .render_aligned(
            page.title,
            Point::new(right_center, title_y),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(WHITE),
            &mut canvas,
        )
        .ok()?;

    let row_height = if items.iter().any(|item| item.detail.is_some()) {
        124
    } else {
        76
    };
    let item_gap = 20;
    let max_rows = ((size.height as i32 - 250) / (row_height + item_gap)).max(1) as usize;
    let rows = items.len().min(max_rows);
    let total_menu_height = rows as i32 * row_height + (rows as i32 - 1).max(0) * item_gap;
    let mut y = (size.height as i32 - total_menu_height) / 2;

    let bar = Size::new((size.width / 3).min(480), row_height as u32);
    let first = selected
        .saturating_sub(rows / 2)
        .min(items.len().saturating_sub(rows));
    for (index, entry) in items.iter().enumerate().skip(first).take(rows) {
        let is_selected = index == selected;
        if is_selected {
            let area = Rectangle::new(Point::new(right_center - bar.width as i32 / 2, y), bar);
            RoundedRectangle::with_equal_corners(area, Size::new(14, 14))
                .into_styled(PrimitiveStyle::with_fill(WHITE))
                .draw(&mut canvas)
                .ok()?;
        }
        let text_color = if is_selected { Rgb888::BLACK } else { WHITE };
        let detail_color = if is_selected { Rgb888::BLACK } else { MUTED };
        let label_y = y + if entry.detail.is_some() {
            42
        } else {
            row_height / 2
        };
        if entry.back {
            let width = item
                .get_rendered_dimensions(entry.label, Point::zero(), VerticalPosition::Center)
                .ok()?
                .advance
                .x;
            let start = right_center - (width + 42) / 2;
            let style = PrimitiveStyle::with_stroke(text_color, 4);
            Line::new(Point::new(start, label_y), Point::new(start + 28, label_y))
                .into_styled(style)
                .draw(&mut canvas)
                .ok()?;
            Line::new(
                Point::new(start, label_y),
                Point::new(start + 10, label_y - 10),
            )
            .into_styled(style)
            .draw(&mut canvas)
            .ok()?;
            Line::new(
                Point::new(start, label_y),
                Point::new(start + 10, label_y + 10),
            )
            .into_styled(style)
            .draw(&mut canvas)
            .ok()?;
            item.render(
                entry.label,
                Point::new(start + 42, label_y),
                VerticalPosition::Center,
                FontColor::Transparent(text_color),
                &mut canvas,
            )
            .ok()?;
        } else {
            item.render_aligned(
                entry.label,
                Point::new(right_center, label_y),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(text_color),
                &mut canvas,
            )
            .ok()?;
        }
        if let Some(detail) = entry.detail {
            hint.render_aligned(
                detail,
                Point::new(right_center, y + 90),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(detail_color),
                &mut canvas,
            )
            .ok()?;
        }
        y += row_height + item_gap;
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
            Point::new(right_center, size.height as i32 - 145),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(MUTED),
            &mut canvas,
        )
        .ok()?;
    }

    render_pair(
        &mut canvas,
        &hint_bold,
        &hint,
        right_center,
        size.height as i32 - 110,
        "VOL+ / VOL- or Arrows",
        " to select",
    )?;
    if page.confirm_hint {
        render_pair(
            &mut canvas,
            &hint_bold,
            &hint,
            right_center,
            size.height as i32 - 75,
            "Power or Enter",
            " to confirm",
        )?;
    }
    canvas.show(&mut output).ok()
}

fn draw_text(page: Page, items: &[Item], selected: usize, countdown: Option<u8>) {
    system::with_stdout(|stdout| {
        let _ = stdout.clear();
        let _ = stdout.enable_cursor(false);
        let _ = writeln!(stdout, "{}\r\n", page.title);
        for (index, item) in items.iter().enumerate() {
            let _ = writeln!(
                stdout,
                "{} {}\r",
                if selected == index { ">" } else { " " },
                if item.back { "<- Back" } else { item.label },
            );
            if let Some(detail) = item.detail {
                let _ = writeln!(stdout, "  {detail}\r",);
            }
        }
        if let Some(countdown) = countdown {
            let _ = writeln!(stdout, "\r\nBooting in {countdown}\r");
        }
        if let Some(device) = page.device {
            let _ = writeln!(stdout, "\r\nSelected Device: {device}\r");
        }
        let _ = writeln!(
            stdout,
            "Bootloader Version: {}\r",
            env!("ARMADA_BOOT_VERSION")
        );
        let _ = writeln!(stdout, "\r\nVOL+ / VOL- or Arrows to select\r");
        if page.confirm_hint {
            let _ = writeln!(stdout, "Power or Enter to confirm\r");
        }
    });
}

fn draw(page: Page, items: &[Item], selected: usize, countdown: Option<u8>) {
    if draw_graphics(page, items, selected, countdown).is_none() {
        draw_text(page, items, selected, countdown);
    }
}

fn keys_down() -> Vec<Key> {
    system::with_stdin(|stdin| {
        let mut down = Vec::new();
        while let Ok(Some(key)) = stdin.read_key() {
            down.push(key);
        }
        let _ = stdin.reset(false);
        boot::stall(Duration::from_millis(2));
        while let Ok(Some(key)) = stdin.read_key() {
            down.push(key);
        }
        down.dedup();
        down
    })
}

fn choose(page: Page, items: &[Item], timed: bool) -> usize {
    let timer = || {
        unsafe { boot::create_event(EventType::TIMER, Tpl::APPLICATION, None, None) }
            .expect("No timer event")
    };
    let (repeat, probe, countdown) = (timer(), timer(), timer());
    let mut held = keys_down();
    if !held.is_empty() {
        let _ = boot::set_timer(&probe, TimerTrigger::Periodic(Duration::from_millis(25)));
    }
    let mut timeout = timed.then_some(3u8);
    if timed {
        let _ = boot::set_timer(&countdown, TimerTrigger::Periodic(Duration::from_secs(1)));
    }
    let mut selected = 0;
    let mut last = None;
    let mut redraw = true;

    'menu: loop {
        if redraw {
            draw(page, items, selected, timeout);
        }
        redraw = false;
        let (index, key) = system::with_stdin(|stdin| {
            let events = unsafe {
                [
                    stdin.wait_for_key_event().expect("No key event"),
                    repeat.unsafe_clone(),
                    probe.unsafe_clone(),
                    countdown.unsafe_clone(),
                ]
            };
            match boot::wait_for_event(&events).unwrap_or(0) {
                0 => (0, stdin.read_key().ok().flatten()),
                index => (index, None),
            }
        });
        let mut keys = Vec::new();
        match index {
            0 => {
                if let Some(key) = key
                    && !held.contains(&key)
                {
                    held.push(key);
                    keys.push(key);
                }
            }
            1 => {
                if let Some(key @ Key::Special(ScanCode::UP | ScanCode::DOWN)) = last {
                    let _ = boot::set_timer(
                        &repeat,
                        TimerTrigger::Relative(Duration::from_millis(100)),
                    );
                    keys.push(key);
                }
            }
            2 => {
                let down = keys_down();
                keys.extend(down.iter().filter(|key| !held.contains(key)));
                held = down;
                if last.is_some_and(|key| !held.contains(&key)) {
                    last = None;
                    let _ = boot::set_timer(&repeat, TimerTrigger::Cancel);
                }
                if held.is_empty() {
                    let _ = boot::set_timer(&probe, TimerTrigger::Cancel);
                }
            }
            _ => {
                if let Some(seconds) = timeout.as_mut() {
                    *seconds -= 1;
                    if *seconds == 0 {
                        break;
                    }
                    redraw = true;
                }
            }
        }
        for key in keys {
            if index != 1 {
                last = Some(key);
                let _ =
                    boot::set_timer(&repeat, TimerTrigger::Relative(Duration::from_millis(300)));
                let _ = boot::set_timer(&probe, TimerTrigger::Periodic(Duration::from_millis(25)));
            }
            match key {
                Key::Special(ScanCode::UP) => {
                    selected = selected.checked_sub(1).unwrap_or(items.len() - 1);
                }
                Key::Special(ScanCode::DOWN) => selected = (selected + 1) % items.len(),
                Key::Special(ScanCode::SUSPEND) => break 'menu,
                Key::Printable(key) if key == '\r' => break 'menu,
                _ => continue,
            }
            timeout = None;
            let _ = boot::set_timer(&countdown, TimerTrigger::Cancel);
            redraw = true;
        }
    }

    let _ = boot::close_event(repeat);
    let _ = boot::close_event(probe);
    let _ = boot::close_event(countdown);
    system::with_stdin(|stdin| while let Ok(Some(_)) = stdin.read_key() {});
    selected
}

pub fn menu(config: &Config, rollback: Option<&str>, device: Option<&str>, timed: bool) -> Choice {
    let mut items = vec![Item {
        label: "ArmadaOS",
        detail: config.version.as_deref(),
        back: false,
    }];
    if let Some(version) = rollback {
        items.push(Item {
            label: "Rollback Version",
            detail: Some(version),
            back: false,
        });
    }
    items.push(Item {
        label: "Select Device",
        detail: Some("Change the current device model"),
        back: false,
    });
    let page = Page {
        config,
        title: "Main Menu",
        device: Some(device.unwrap_or("Unknown")),
        confirm_hint: true,
    };
    match choose(page, &items, timed) {
        0 => Choice::Armada,
        1 if rollback.is_some() => Choice::Previous,
        _ => Choice::Device,
    }
}

pub fn device_menu(
    config: &Config,
    models: &[&str],
    current: Option<&str>,
    cancellable: bool,
) -> Option<usize> {
    let mut manufacturers = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let manufacturer = model.split_once(' ').map_or(*model, |(name, _)| name);
        if manufacturers.last().map(|(name, _)| *name) != Some(manufacturer) {
            manufacturers.push((manufacturer, index));
        }
    }

    loop {
        let names: Vec<_> = manufacturers
            .iter()
            .map(|(name, _)| name.to_ascii_uppercase())
            .collect();
        let mut items: Vec<_> = names.iter().map(|name| Item::new(name)).collect();
        if cancellable {
            items.push(Item::back());
        }
        let page = Page {
            config,
            title: "Select Device",
            device: current,
            confirm_hint: true,
        };
        let manufacturer = choose(page, &items, false);
        if cancellable && manufacturer == manufacturers.len() {
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
        items.push(Item::back());
        let selected = choose(page, &items, false);
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
