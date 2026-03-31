#![deny(unreachable_patterns)]

use crate::vm::Vm;
use polkavm::ProgramBlob;
use sdl2::event::Event;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use std::io::Write;
use std::rc::Rc;
use std::str::FromStr;

mod keys;
mod vm;

fn main() {
    env_logger::init();

    let mut program_override = None;
    let mut rom_override = None;
    let mut rec_file: Option<std::fs::File> = None;
    for arg in std::env::args().skip(1) {
        if arg.starts_with("--REC=") {
            let path_str = &arg[6..];
            let path_buf = std::path::PathBuf::from_str(&path_str).unwrap();
            println!("Recording in {}", path_buf.to_str().unwrap());
            let file = std::fs::File::create(&path_buf).unwrap();
            rec_file = Some(file);
        } else {
            let bytes = std::fs::read(arg).unwrap();
            if bytes.starts_with(b"PVM\0") {
                program_override = Some(bytes);
            } else {
                rom_override = Some(bytes);
            }
        }
    }

    const DOOM_PROGRAM: &[u8] = include_bytes!("../roms/doom.polkavm");
    const DOOM_ROM: &[u8] = include_bytes!("../roms/doom1.wad");

    let blob = ProgramBlob::parse(program_override.as_deref().unwrap_or(DOOM_PROGRAM).into()).unwrap();
    let mut vm = Vm::from_blob(blob).unwrap();

    vm.initialize(rom_override.as_deref().unwrap_or(DOOM_ROM)).unwrap();

    let sdl_context = sdl2::init().unwrap();
    let video_context = sdl_context.video().unwrap();
    let audio_context = sdl_context.audio().unwrap();
    let mut event_pump = sdl_context.event_pump().unwrap();
    let window = video_context
        .window("polkadoom", 640, 400)
        .position_centered()
        .resizable()
        .build()
        .unwrap();

    let mut canvas = window.into_canvas().build().unwrap();
    let texture_creator = canvas.texture_creator();
    let mut texture = None;

    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    canvas.present();

    let audio_queue = audio_context
        .open_queue::<i16, Option<&str>>(
            None,
            &sdl2::audio::AudioSpecDesired {
                freq: Some(44100),
                channels: Some(2),
                samples: Some(512),
            },
        )
        .unwrap();

    let audio_queue = Rc::new(audio_queue);
    audio_queue.resume();

    let queue = audio_queue.clone();
    vm.set_on_audio_frame(move |buffer| {
        let _ = queue.queue_audio(buffer);
    });

    let mut tick_nb: u32 = 0;

    if let Some(rec) = rec_file.as_mut() {
        // always start with tick (we peak on this then process).
        rec.write_all(&RecordAction::Start.with_data(tick_nb, 0).to_le_bytes()).unwrap();
        rec.flush().unwrap();
    }

    let mut keys: [isize; 256] = [0; 256];
    loop {
        loop {
            while let Some(event) = event_pump.poll_event() {
                let key_change = match event {
                    Event::Quit { .. } => {
                        std::process::exit(0);
                    }
                    Event::KeyDown {
                        keycode: Some(keycode),
                        repeat,
                        ..
                    } if !repeat => crate::keys::from_sdl2(keycode).map(|key| (key, true)),
                    Event::KeyUp {
                        keycode: Some(keycode),
                        repeat,
                        ..
                    } if !repeat => crate::keys::from_sdl2(keycode).map(|key| (key, false)),
                    Event::MouseButtonDown {
                        mouse_btn: sdl2::mouse::MouseButton::Left,
                        ..
                    } => Some((crate::keys::FIRE, true)),
                    Event::MouseButtonUp {
                        mouse_btn: sdl2::mouse::MouseButton::Left,
                        ..
                    } => Some((crate::keys::FIRE, false)),
                    Event::MouseButtonDown {
                        mouse_btn: sdl2::mouse::MouseButton::Right,
                        ..
                    } => Some((crate::keys::ALT, true)),
                    Event::MouseButtonUp {
                        mouse_btn: sdl2::mouse::MouseButton::Right,
                        ..
                    } => Some((crate::keys::ALT, false)),
                    Event::MouseButtonDown {
                        mouse_btn: sdl2::mouse::MouseButton::Middle,
                        ..
                    } => Some((crate::keys::USE, true)),
                    Event::MouseButtonUp {
                        mouse_btn: sdl2::mouse::MouseButton::Middle,
                        ..
                    } => Some((crate::keys::USE, false)),
                    _ => None,
                };

                if let Some((key, is_pressed)) = key_change {
                    let before = keys[key as usize] > 0;
                    if is_pressed {
                        keys[key as usize] += 1;
                    } else {
                        keys[key as usize] -= 1;
                    }

                    let after = keys[key as usize] > 0;
                    if before != after {
                        vm.on_keychange(key, after).unwrap();
                        if let Some(rec) = rec_file.as_mut() {
                            if after {
                                rec.write_all(&RecordAction::KeyPressed.with_data(tick_nb, key as u32).to_le_bytes())
                                    .unwrap();
                            } else {
                                rec.write_all(&RecordAction::KeyReleased.with_data(tick_nb, key as u32).to_le_bytes())
                                    .unwrap();
                            }
                            rec.flush().unwrap();
                        }
                    }
                }
            }

            let samples_queued = audio_queue.size() / 4;
            let samples_per_millisecond = 44100.0 / 1000.0;
            let milliseconds_queued = samples_queued as f32 / samples_per_millisecond as f32;
            if milliseconds_queued < 32.0 {
                break;
            }

            std::thread::sleep(core::time::Duration::from_millis(1));
        }

        let Ok((width, height, frame)) = vm.run_for_a_frame() else {
            break;
        };
        tick_nb = tick_nb.wrapping_add(1);

        canvas.clear();
        if !frame.is_empty() {
            if let Some((_, texture_width, texture_height)) = texture {
                if width != texture_width || height != texture_height {
                    texture = None;
                }
            }

            let (texture, tex_width, tex_height) = if let Some((ref mut texture, width, height)) = texture {
                (texture, width, height)
            } else {
                let tex = texture_creator
                    .create_texture_streaming(PixelFormatEnum::ARGB8888, width, height)
                    .unwrap();

                texture = Some((tex, width, height));
                (&mut texture.as_mut().unwrap().0, width, height)
            };

            let (display_width, display_height) = canvas.output_size().unwrap();
            let aspect = tex_width as f32 / tex_height as f32;
            let out_width = core::cmp::min(display_width, (display_height as f32 * aspect) as u32);

            texture.update(None, frame, width as usize * 4).unwrap();
            canvas
                .copy(
                    texture,
                    None,
                    Some(Rect::new(((display_width - out_width) / 2) as i32, 0, out_width, display_height)),
                )
                .unwrap();
        }

        canvas.present();
    }

    if let Some(rec) = rec_file.as_mut() {
        rec.flush().unwrap();
    }
}

#[derive(Debug)]
pub enum RecordAction {
    Start = 0,
    // not sure of any use? (can add manually and split precessing by ticks?, then start at split
    // ticks for next sending. -> can make sense if sending every n seconds/ticks as workitem (need
    // modify this client to send/cast in addition to write to file (can then record payload).
    // TODO is it easy to suspend sdl too: likely yes (would need proper sound suspend though).
    Suspend = 1,
    KeyPressed = 2,
    KeyReleased = 3,
}

type TickCount = u32;

impl RecordAction {
    pub fn with_data(self, tick: TickCount, data: u32) -> u64 {
        if (data as u32) & !(u32::MAX >> 2) != 0 {
            // data type to large (content over record encoding)
            // `with_data` should only be call on record, panicking
            // in record due to a bug is fine.
            panic!("data record too large");
        }
        ((tick as u64) << 32) | ((self as u64) << 30) | (data as u64)
    }
    pub fn read_data(data: u64) -> (TickCount, RecordAction, u32) {
        let tick = (data >> 32) as u32;
        let data = data as u32;
        match data >> 30 {
            val if val == RecordAction::Start as u32 => (tick, RecordAction::Start, 0),
            val if val == RecordAction::Suspend as u32 => (tick, RecordAction::Suspend, 0),
            val if val == RecordAction::KeyPressed as u32 => (tick, RecordAction::KeyPressed, data & (u32::MAX >> 2)),
            val if val == RecordAction::KeyReleased as u32 => (tick, RecordAction::KeyReleased, data & (u32::MAX >> 2)),
            // Start at tick 0 is safe error place holder (as it warrants a reset of running
            // process).
            _ => (0, RecordAction::Start, 0),
        }
    }
}

#[test]
fn dummy_test_just_disp() {
    use std::io::Read;
    let path_buf = std::path::PathBuf::from_str(&"./log_rec").unwrap();
    let mut file = std::fs::File::open(&path_buf).unwrap();
    let mut buf = [0u8; 8];
    loop {
        file.read_exact(&mut buf).unwrap();
        let data = u64::from_le_bytes(buf);
        let (tick, action, key) = RecordAction::read_data(data);
        println!("{}: {:?} {}", tick, action, key);
    }
}
