use idol_appreciation_club_backend::img::{decode, init, resize_exact, webp_bytes};
use libvips::{ops, VipsImage};

fn red_png(w: u32, h: u32) -> Vec<u8> {
    let black = ops::black(w as i32, h as i32).unwrap();
    let img = ops::bandjoin_const(&black, &mut [220.0, 40.0, 40.0, 255.0]).unwrap();
    ops::pngsave_buffer(&img).unwrap()
}

#[test]
fn harness_vips_buffer_saves() {
    init().unwrap();
    let png = red_png(100, 50);
    println!("png {} bytes", png.len());
    let (img1, _) = decode(&png).unwrap();
    let d1 = resize_exact(&img1, 32, 16).unwrap();
    println!("png save: {:?}", d1.image_write_to_buffer(".png").as_ref().map(|b| b.len()));
    println!("webp save: {:?}", d1.image_write_to_buffer(".webp").as_ref().map(|b| b.len()));
    assert!(webp_bytes(&d1, 85).is_ok());
}
