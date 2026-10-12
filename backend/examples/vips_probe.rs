//! Container debug probe: isolates which vips op breaks webp buffering in
//! minimal Debian builds. Run: cargo run --example vips_probe

use libvips::{ops, VipsApp, VipsImage};

fn main() {
    let app = VipsApp::new("vips_probe", true).expect("init");
    app.concurrency_set(1);

    let black = ops::black(100i32, 50).unwrap();
    println!("step1 black: bands={} w={} h={}", black.get_bands(), black.get_width(), black.get_height());

    let img = ops::bandjoin_const(&black, &mut [220.0f64, 40.0, 40.0, 255.0]).unwrap();
    println!("step2 bandjoin: bands={}", img.get_bands());

    match ops::pngsave_buffer(&img) {
        Ok(png) => println!("step3 pngsave: {} bytes", png.len()),
        Err(e) => {
            println!("step3 pngsave FAILED: {e:?}");
            return;
        }
    }

    let png = match ops::pngsave_buffer(&img) {
        Ok(p) => p,
        Err(e) => return,
    };
    let dec = VipsImage::new_from_buffer(&png, "").unwrap();
    println!("step4 decode: bands={} w={} h={}", dec.get_bands(), dec.get_width(), dec.get_height());

    let pm = match ops::premultiply(&dec) {
        Ok(p) => p,
        Err(e) => {
            println!("step5 premultiply FAILED: {e:?}");
            return;
        }
    };
    let rs = match ops::resize(&pm, 0.32) {
        Ok(r) => r,
        Err(e) => {
            println!("step6 resize FAILED: {e:?}");
            return;
        }
    };
    let up = match ops::unpremultiply(&rs) {
        Ok(u) => u,
        Err(e) => {
            println!("step7 unpremultiply FAILED: {e:?}");
            return;
        }
    };
    println!("step7: w={} h={} bands={}", up.get_width(), up.get_height(), up.get_bands());

    match up.image_write_to_buffer(".webp[Q=85]") {
        Ok(b) => println!("step8 webp OK: {} bytes", b.len()),
        Err(e) => println!("step8 webp FAILED: {e:?}"),
    }

    // Direct save without the premultiply pipeline:
    match dec.image_write_to_buffer(".webp[Q=85]") {
        Ok(b) => println!("step9 raw-decode webp OK: {} bytes", b.len()),
        Err(e) => println!("step9 raw-decode webp FAILED: {e:?}"),
    }
}
