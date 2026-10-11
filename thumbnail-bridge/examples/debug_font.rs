use plotmill_bridge::{fontmatch, ocr::Ocr, read_image, style::analyze};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let img = read_image(std::path::Path::new(&a[1])).unwrap();
    let ocr = Ocr::load().unwrap();
    let faces = fontmatch::japanese_faces();
    for l in ocr.read(&img).unwrap() {
        if let Some(st) = analyze(&img, &l) {
            println!("{} angle={:.4} rect={:?} fill_bbox={:?}", l.text, l.rect.angle, l.rect, st.fill_bbox);
            for f in fontmatch::fit_line(&l, &st, &faces, 3) {
                println!("   {} {} {:.1}px tr={:.0} score={:.3} anchor={:?} text={}", f.face.family, f.face.style, f.size_px, f.tracking, f.score, f.anchor, f.text);
            }
        }
    }
}
