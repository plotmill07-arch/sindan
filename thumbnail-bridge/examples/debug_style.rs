//! Dump per-line analysis masks as PGM for debugging: out/debug/<n>_{ink,fill,hole}.pgm
use plotmill_bridge::{ocr::Ocr, read_image, style::analyze};
use std::io::Write;
fn pgm(path: &str, w: usize, h: usize, v: &[bool]) {
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "P5\n{w} {h}\n255\n").unwrap();
    f.write_all(&v.iter().map(|b| if *b { 255u8 } else { 0 }).collect::<Vec<_>>()).unwrap();
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let img = read_image(std::path::Path::new(&a[1])).unwrap();
    let ocr = Ocr::load().unwrap();
    std::fs::create_dir_all("out/debug").unwrap();
    for (i, l) in ocr.read(&img).unwrap().iter().enumerate() {
        println!("{i}: {} {:?} vertical={} pos={:?}", l.text, l.rect, l.vertical, l.char_pos);
        if let Some(s) = analyze(&img, l) {
            let (w, h) = (s.cw(), s.ch());
            pgm(&format!("out/debug/{i}_ink.pgm"), w, h, &s.ink_mask);
            pgm(&format!("out/debug/{i}_fill.pgm"), w, h, &s.fill_mask);
            pgm(&format!("out/debug/{i}_hole.pgm"), w, h, &s.hole);
            println!("   crop {:?} fill {:?} stroke {:?}", s.crop, s.fill, s.stroke);
        }
    }
}
