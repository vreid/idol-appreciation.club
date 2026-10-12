//! Container debug probe: runs the EXACT test pipeline through the crate
//! (crate init + decode + resize_exact + webp_bytes) to isolate the env
//! difference that makes webp buffering fail in minimal Debian builds.
//! Run: cargo run --example vips_probe

use idol_appreciation_club_backend::img::{decode, init, resize_exact, webp_bytes};
use libvips::{ops, VipsImage};

fn red_png(w: u32, h: u32) -> Vec<u8> {
    let black = ops::black(w as i32, h as i32).unwrap();
    let img = ops::bandjoin_const(&black, &mut [220.0, 40.0, 40.0, 255.0]).unwrap();
    ops::pngsave_buffer(&img).unwrap()
}

fn main() {
    // Reproduce the test harness: init + ops from a SPAWNED thread.
    let h = std::thread::spawn(|| run("thread"));
    h.join().unwrap();
    run("main");
}

fn run(tag: &str) {
    init().unwrap();
    println!("=== {tag} ===");

    let png = red_png(100, 50);
    println!("step1 png: {} bytes", png.len());

    let (img1, fmt) = match decode(&png) {
        Ok(v) => v,
        Err(e) => {
            println!("step2 decode FAILED: {e:?}");
            return;
        }
    };
    println!("step2 decode ok: format={fmt:?} bands={} w={} h={}",
        img1.get_bands(), img1.get_width(), img1.get_height());

    let d1 = match resize_exact(&img1, 32, 16) {
        Ok(v) => v,
        Err(e) => {
            println!("step3 resize_exact FAILED: {e:?}");
            return;
        }
    };
    println!("step3 resize_exact ok: bands={} w={} h={}",
        d1.get_bands(), d1.get_width(), d1.get_height());

    match webp_bytes(&d1, 85) {
        Ok(b) => println!("step4 webp OK: {} bytes", b.len()),
        Err(e) => println!("step4 webp FAILED: {e:?}"),
    }

    // Same encode after full materialization:
    let solid = match VipsImage::image_copy_memory(d1) {
        Ok(v) => v,
        Err(e) => {
            println!("step5 copy_memory FAILED: {e:?}");
            return;
        }
    };
    match webp_bytes(&solid, 85) {
        Ok(b) => println!("step5 materialized webp OK: {} bytes", b.len()),
        Err(e) => println!("step5 materialized webp FAILED: {e:?}"),
    }
}
