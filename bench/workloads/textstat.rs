// Same algorithm as textstat.nx: build a corpus from a fixed vocabulary,
// tokenize it by scanning for separators, count words in a map, and print a
// report with a hand-written integer-to-string conversion.
//
// `BTreeMap` rather than `HashMap` because the report walks the vocabulary
// in a fixed order rather than iterating the map, and every aggregate over
// the map is order-independent -- so either map would do, and the ordered
// one keeps the iteration-order question out of the correctness gate.

use std::collections::BTreeMap;

fn itoa(n: i64) -> String {
    let digits = b"0123456789";
    if n == 0 {
        return String::from("0");
    }
    if n < 0 {
        return format!("-{}", itoa(-n));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut n = n;
    while n > 0 {
        out.push(digits[(n % 10) as usize]);
        n /= 10;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn main() {
    let reps: usize = 1200;

    let letters: Vec<String> = "abcdefghijklmnopqrstuvwxyz".chars().map(|c| c.to_string()).collect();

    let nwords: usize = 40;
    let mut vocab: Vec<String> = Vec::with_capacity(nwords);
    let mut i: usize = 0;
    while i < nwords {
        let w: String = format!(
            "{}{}{}{}",
            letters[i % 26],
            letters[i / 26 % 26],
            letters[i / 676 % 26],
            letters[i % 5]
        );
        vocab.push(w);
        i += 1;
    }

    let mut para = String::new();
    let mut i: usize = 0;
    while i < 200 {
        if i > 0 {
            para.push(' ');
        }
        para.push_str(&vocab[i * 7919 % nwords]);
        i += 1;
    }
    para.push_str(", ");

    let mut corpus = String::new();
    let mut r: usize = 0;
    while r < reps {
        corpus.push_str(&para);
        r += 1;
    }

    let corpus_len = corpus.len();
    println!("{}", corpus_len);

    let alpha = b"abcdefghijklmnopqrstuvwxyz";
    let cb = corpus.as_bytes();

    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut tokens: i64 = 0;
    let mut longest: i64 = 0;
    let mut cur: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < corpus_len {
        let ch = cb[i];
        if alpha.contains(&ch) {
            cur.push(ch);
            if cur.len() as i64 > longest {
                longest = cur.len() as i64;
            }
        } else {
            if !cur.is_empty() {
                tokens += 1;
                let key = String::from_utf8(std::mem::take(&mut cur)).unwrap();
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        i += 1;
    }
    if !cur.is_empty() {
        tokens += 1;
        let key = String::from_utf8(cur.clone()).unwrap();
        *counts.entry(key).or_insert(0) += 1;
    }

    println!("{} {} {}", tokens, counts.len(), longest);

    let mut report = String::new();
    let mut i: usize = 0;
    while i < nwords {
        if let Some(c) = counts.get(&vocab[i]) {
            report.push_str(&vocab[i]);
            report.push('=');
            report.push_str(&itoa(*c));
            report.push(' ');
        }
        i += 1;
    }
    println!("{}", report);

    let mut wsum: i64 = 0;
    for (w, c) in counts.iter() {
        wsum += (w.len() as i64) * *c;
    }
    println!("{}", wsum);

    let mut best: i64 = 0;
    let mut bestn: i64 = 0;
    let mut i: usize = 0;
    while i < nwords {
        if let Some(c) = counts.get(&vocab[i]) {
            if *c > best {
                best = *c;
            }
            if *c == best {
                bestn += 1;
            }
        }
        i += 1;
    }
    println!("{} {}", best, bestn);

    println!(
        "{} {} {} {} {}",
        itoa(0),
        itoa(7),
        itoa(1234),
        itoa(1000000),
        itoa(-42)
    );
}