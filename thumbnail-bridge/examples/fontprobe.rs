use photocraft_doc::TextLayer;
use photocraft_doc::text::{CharStyle, TextRun};
fn main() {
    let mut eng = photocraft_text::shared().lock().unwrap();
    let fams = eng.fonts.families();
    println!("{} families", fams.len());
    for fam in fams.iter().filter(|f| f.contains("IPA") || f.contains("BIZ") || f.contains("Inter") || f.contains("Noto") || f.contains("WenQ")) {
        let t = "Aあア漢";
        let probe = TextLayer { text: t.into(), runs: vec![TextRun { len: t.len(), style: CharStyle { font_family: fam.clone(), size_pt: 20.0, ..Default::default() } }], ..Default::default() };
        let l = eng.layout(&probe, 72.0);
        println!("{fam}: faces={} glyphs={} ids={:?}", l.faces.len(), l.glyphs.len(), l.faces.iter().map(|f| f.font.data.id()).collect::<Vec<_>>());
    }
}
