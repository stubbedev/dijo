use super::*;

fn pages(program: &[u8]) -> Vec<Page> {
    let mut pages = Vec::new();
    run(program, &mut |page| {
        pages.push(page);
        Ok(())
    })
    .unwrap();
    pages
}

fn texts(program: &[u8]) -> Vec<Vec<String>> {
    pages(program)
        .into_iter()
        .map(|page| page.text.paragraphs)
        .collect()
}

#[test]
fn shows_strings_page_by_page() {
    let program = b"/Times-Roman findfont 12 scalefont setfont\n\
        72 720 moveto (The keeper) show ( climbed) show\n\
        72 706 moveto (the stairs.) show showpage\n\
        72 720 moveto <4E6967687466> show showpage";
    assert_eq!(
        texts(program),
        [
            vec!["The keeper climbed\nthe stairs.".to_owned()],
            vec!["Nightf".to_owned()]
        ]
    );
}

#[test]
fn spaces_words_by_the_standard_fonts_widths() {
    let program = b"/Helvetica findfont 10 scalefont setfont\n\
        100 500 moveto (Light) show 124.47 500 moveto (house) show\n\
        100 488 moveto (sto) show 114 488 moveto (rm) show";
    assert_eq!(texts(program), [vec!["Light house\nstorm".to_owned()]]);
}

#[test]
fn follows_show_aliases_and_relative_moves() {
    let program = b"/N {def} def /B {bind def} N\n\
        /S /show load def /p {show} B /a {moveto} B /w {0 rmoveto} B\n\
        /F {findfont exch scalefont setfont} B\n\
        10 /Courier F 50 400 a (Every) p 6 w (evening) S -0.5 w (s) p\n\
        /T {moveto show} B (dusk) 50 388 T";
    assert_eq!(texts(program), [vec!["Every evenings\ndusk".to_owned()]]);
}

#[test]
fn reencodes_fonts_as_programs_do() {
    let program = b"/reencode { findfont dup length dict begin\n\
          { 1 index /FID ne { def } { pop pop } ifelse } forall\n\
          /Encoding ISOLatin1Encoding def currentdict end definefont pop } bind def\n\
        /Times-ISO /Times-Roman reencode\n\
        /Times-ISO findfont 11 scalefont setfont 72 700 moveto (Fyrt\\345rn \\251 \\047) show\n\
        /Custom 12 dict begin /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def\n\
        /FontBBox [0 0 1000 1000] def\n\
        /Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for\n\
        Encoding 1 /K put Encoding 2 /e put Encoding 3 /uni00F8 put\n\
        currentdict end definefont 20 scalefont setfont 72 600 moveto <01020203> show";
    assert_eq!(
        texts(program),
        [vec!["Fyrtårn © ’".to_owned(), "Keeø".to_owned()]]
    );
}

#[test]
fn reads_the_encoding_of_an_embedded_type_1_font() {
    let program = b"%!PS-AdobeFont-1.0: Keeper 001\n\
        11 dict begin /FontName /Keeper def /FontType 1 def\n\
        /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
        /Encoding 256 array 0 1 255 {1 index exch /.notdef put} for\n\
        dup 65 /L put dup 66 /i put dup 67 /g put dup 68 /h put dup 69 /t put\n\
        readonly def currentdict end currentfile eexec\n\
        d9d66f633b846a989b9974b0179fc6cc445bc2c03103c68570a7b354a4a280ae\n\
        0000000000000000000000000000000000000000000000000000000000000000\n\
        cleartomark\n\
        /Keeper findfont 10 scalefont setfont 10 10 moveto (ABCDE) show showpage";
    assert_eq!(texts(program), [vec!["Light".to_owned()]]);
}

#[test]
fn ends_unshown_pages_at_their_structuring_marks() {
    let program = b"%!PS-Adobe-3.0\n/Courier findfont 10 scalefont setfont\n\
        /draw { moveto show } def /this 1 dict def //this /y 5 put\n\
        %%Page: 1 1\n(first) 10 10 draw { (unclosed\n\
        %%Page: 2 2\n(second) 10 10 draw showpage\n\
        %%Page: 3 3\n(third) 10 //this /y get draw\n%%Trailer\n";
    let pages = pages(program);
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].faults, [Fault::UnfollowedPage]);
    assert_eq!(pages[0].text.paragraphs, ["first"]);
    assert_eq!(pages[1].faults, []);
    assert_eq!(pages[1].text.paragraphs, ["second"]);
    assert_eq!(pages[2].faults, [Fault::UnfollowedPage]);
    assert_eq!(pages[2].text.paragraphs, ["third"]);
}

#[test]
fn undoes_definitions_at_restore() {
    let program = b"/Courier findfont 10 scalefont setfont\n\
        save /showpage {} def 10 10 moveto (included) show showpage restore\n\
        10 0 moveto (after) show showpage";
    assert_eq!(texts(program), [vec!["included\nafter".to_owned()]]);
}

#[test]
fn passes_image_data_read_from_the_program() {
    let program = b"/Courier findfont 10 scalefont setfont /buffer 2 string def\n\
        4 2 8 [1 0 0 1 0 0] {currentfile buffer readhexstring pop} image\n\
        (show)(show)\n0123456789abcdef\n\
        10 10 moveto (after) show\n\
        8 1 8 [1 0 0 1 0 0] currentfile image\n\
        (show) showpage\n\
        /DataSource currentfile /ASCII85Decode filter def\n\
        << /Width 2 /Height 2 /BitsPerComponent 8 /Decode [0 1] /DataSource DataSource >>\n\
        image\n(ab) show <~ (x) show ~>\n10 0 moveto (end) show";
    assert_eq!(texts(program), [vec!["after\nend".to_owned()]]);
}

#[test]
fn records_what_it_cannot_resolve() {
    let program = b"10 10 moveto (no font) show\n\
        /Courier findfont 10 scalefont setfont 1 2 add show\n\
        /Composite << /FontType 0 /FontMatrix [1 0 0 1 0 0] >> definefont setfont\n\
        (cid) show showpage\n\
        /Courier findfont 10 scalefont setfont 10 10 moveto (fine) show showpage";
    let pages = pages(program);
    assert_eq!(
        pages[0].faults,
        [
            Fault::UnknownFont,
            Fault::ComputedString,
            Fault::CompositeFont
        ]
    );
    assert_eq!(pages[0].text.glyphs, 0);
    assert_eq!(pages[1].faults, []);
    assert_eq!(pages[1].text.paragraphs, ["fine"]);
}

#[test]
fn stops_a_program_that_runs_past_its_steps() {
    let program = b"/Courier findfont 10 scalefont setfont 10 10 moveto (before) show\n\
        {} loop (after) show";
    let pages = pages(program);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].faults, [Fault::ExecutionLimit]);
    assert_eq!(pages[0].text.paragraphs, ["before"]);
}

#[test]
fn lists_each_page_marked_after_the_program_is_stopped() {
    let program = b"/Courier findfont 10 scalefont setfont\n%%Page: 1 1\n\
        10 10 moveto (before) show {} loop\n%%Page: 2 2\n(never) show showpage\n%%EOF";
    let pages = pages(program);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].faults, [Fault::ExecutionLimit]);
    assert_eq!(pages[0].text.paragraphs, ["before"]);
    assert_eq!(pages[1].faults, [Fault::ExecutionLimit]);
    assert_eq!(pages[1].text.glyphs, 0);
}

#[test]
fn stops_a_program_that_runs_past_its_memory() {
    let program = b"/a [] def 1 1 100000 { pop /a [ a 65536 string ] def } for";
    assert_eq!(pages(program)[0].faults, [Fault::ExecutionLimit]);
}

#[test]
fn survives_recursion_and_stack_abuse() {
    let program = b"/r { r } def r /Courier findfont 10 scalefont setfont\n\
        1 1 70000 { } for clear pop pop exch roll index\n\
        10 10 moveto (alive) show";
    assert_eq!(texts(program), [vec!["alive".to_owned()]]);
}

#[test]
fn evaluates_conditions_and_loops() {
    let program = b"/Courier findfont 10 scalefont setfont 10 10 moveto\n\
        languagelevel 2 ge { (two) } { (one) } ifelse show\n\
        /s 3 string def 0 1 2 { s exch dup 97 add put } for s show\n\
        [ (x) (y) ] { show } forall\n\
        3 { (z) show } repeat\n\
        { (w) show exit (never) show } loop\n\
        { (q) show stop } stopped { (!) show } if\n\
        /n 5 def n 2 mod 1 eq n 10 lt and { (odd) show } if\n\
        12 s cvs show";
    assert_eq!(texts(program), [vec!["twoabcxyzzzwq!odd12".to_owned()]]);
}
