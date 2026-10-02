// Same algorithm as strbuild.nx.
//
// The one place the two differ on purpose: Rust builds a string with
// `push_str`, which is amortised O(1) and reuses one buffer. NX has no
// mutable string at all, so `out = out + ws[i]` is the only spelling
// available and allocates a new buffer every time. The row therefore
// measures a missing stdlib feature, not a codegen difference.

fn main() {
    let n: usize = 12000;

    let vocab = [
        "alpha", "bravo", "delta", "echo", "fox", "golf", "hotel", "india", "juliet", "kilo",
        "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
        "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "amber", "basalt", "cobalt",
        "dune", "ember", "fjord", "garnet",
    ];

    let mut words: Vec<&str> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        words.push(vocab[i % 32]);
        i += 1;
    }

    fn join(ws: &[&str], sep: &str) -> String {
        let mut out = String::new();
        let mut i = 0usize;
        while i < ws.len() {
            if i > 0 {
                out.push_str(sep);
            }
            out.push_str(ws[i]);
            i += 1;
        }
        out
    }

    let text: Vec<u8> = join(&words, " ").into_bytes();

    println!("{}", text.len());
    println!("{}", text[0] as char);
    println!("{}", text[text.len() - 1] as char);

    let mid = &text[100..200];
    println!("{} {} {}", mid.len(), mid[0] as char, mid[99] as char);

    let mut u: Vec<u8> = Vec::new();
    let mut j = 0usize;
    while j < 4 {
        u.extend_from_slice(&text);
        j += 1;
    }
    println!("{} {} {}", u.len(), u[999] as char, u[1000] as char);

    let mut acc: Vec<u8> = Vec::new();
    let mut k = 0usize;
    while k < 4000 {
        acc.push(text[k]);
        k += 1;
    }
    println!("{} {} {}", acc.len(), acc[0] as char, acc[3999] as char);
}