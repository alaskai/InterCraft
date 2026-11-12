// Plain explanations of the design terms the model uses, shown when the pointer is
// over a step that mentions them. Written for someone who has never used CAD.

/// (term, what it means). Lower case. Longer phrases first, so "clearance hole"
/// is found before "clearance".
pub const TERMS: &[(&str, &str)] = &[
    ("clearance hole", "A hole a little bigger than the screw, so the screw passes through freely and only grips in the part below."),
    ("pilot hole", "A small hole drilled first, so a screw can cut its own thread without splitting the part."),
    ("press fit", "A peg or bearing made very slightly bigger than its hole, so it has to be pushed in and then stays put."),
    ("sliding fit", "A small gap between two parts, so one slides or turns in the other without rattling."),
    ("wall thickness", "How thick the material between two surfaces is. Too thin and a printed part is weak or won't print."),
    ("heat-set insert", "A small brass sleeve with an inner thread, melted into a printed hole with a soldering iron so screws grip metal, not plastic."),
    ("split clamp", "A slot cut through a hole so a screw can squeeze the hole tighter around a rod."),
    ("fillet", "A rounded edge. Inside corners get stronger, outside corners get nicer to hold."),
    ("chamfer", "A flat, angled cut across an edge. Takes off sharpness and helps parts slide together."),
    ("boss", "A raised round bump, usually around a hole, that gives a screw more material to grip."),
    ("rib", "A thin wall added for stiffness, like the ridges under a plastic lid."),
    ("countersink", "A cone-shaped widening at the top of a hole, so a flat-headed screw sits flush with the surface."),
    ("counterbore", "A wider, flat-bottomed step at the top of a hole, so a screw head sits below the surface."),
    ("clearance", "The deliberate gap between two parts that have to fit together."),
    ("tolerance", "How far a real size may be from the designed size and still work. Printers are usually within about 0.2 mm."),
    ("bore", "A round hole, often for a rod, shaft or bearing to sit in."),
    ("slot", "A long, narrow opening, often so a screw position can be adjusted."),
    ("pocket", "A recess cut into a face that doesn't go all the way through."),
    ("through hole", "A hole that goes all the way through the part."),
    ("blind hole", "A hole that stops partway, so it doesn't come out the other side."),
    ("overhang", "A part of the shape that sticks out over nothing. A 3D printer has to print it on air, so steep ones need supports."),
    ("support", "Extra material a 3D printer adds under overhangs, removed after printing."),
    ("infill", "The pattern printed inside a part instead of solid plastic. More infill is stronger and heavier."),
    ("extrude", "Push a flat outline straight out into a solid, like pressing dough through a shape."),
    ("revolve", "Spin a flat outline around an axis to make a round solid, like a vase on a pottery wheel."),
    ("shell", "Hollow a solid out, leaving walls of a set thickness."),
    ("datum", "A fixed reference point, line or face that other sizes are measured from."),
    ("thread", "The spiral ridge on a screw or inside a nut."),
    ("standoff", "A short post that holds one part a set distance above another, like a circuit board above a case."),
    ("draft", "A slight taper on a wall so a part comes out of a mould easily."),
];

/// The glossary terms that appear in some text, each once, in the order of TERMS.
/// Matches whole words, plurals included ("fillets" finds "fillet").
pub fn terms_in(text: &str) -> Vec<(&'static str, &'static str)> {
    let lower = text.to_lowercase();
    let mut found: Vec<(&'static str, &'static str)> = Vec::new();
    for &(term, meaning) in TERMS {
        // A shorter term inside one already found ("clearance" in "clearance hole") adds nothing.
        if found.iter().any(|(longer, _)| longer.contains(term)) {
            continue;
        }
        if contains_word(&lower, term) {
            found.push((term, meaning));
        }
    }
    found
}

fn contains_word(text: &str, term: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric();
    let mut from = 0;
    while let Some(at) = text[from..].find(term) {
        let start = from + at;
        let end = start + term.len();
        let before_ok = text[..start].chars().next_back().is_none_or(|c| !is_word(c));
        // Allow a plural "s" or "es" straight after the term.
        let rest = &text[end..];
        let rest = rest.strip_prefix("es").or_else(|| rest.strip_prefix('s')).unwrap_or(rest);
        let after_ok = rest.chars().next().is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Hover text explaining the terms in `text`, or None when it has none.
pub fn hover_for(text: &str) -> Option<String> {
    let found = terms_in(text);
    if found.is_empty() {
        return None;
    }
    let lines: Vec<String> = found.iter().map(|(term, meaning)| format!("{}: {meaning}", capitalise(term))).collect();
    Some(lines.join("\n\n"))
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_whole_words_and_plurals_once() {
        let found: Vec<&str> = terms_in("Edge fillets, a Boss and two clearance holes.").iter().map(|(t, _)| *t).collect();
        assert_eq!(found, ["clearance hole", "fillet", "boss"], "clearance alone isn't listed again");
        assert!(terms_in("Bossanova and unthreaded").is_empty(), "parts of other words don't count");
        assert!(terms_in("Base block").is_empty());
        assert!(hover_for("Rod bore").unwrap().starts_with("Bore: A round hole"));
    }

    #[test]
    fn every_entry_is_plain_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (term, meaning) in TERMS {
            assert!(seen.insert(*term), "{term} is listed twice");
            assert_eq!(*term, term.to_lowercase());
            assert!(meaning.ends_with('.') && meaning.len() < 200, "{term}");
        }
    }
}
