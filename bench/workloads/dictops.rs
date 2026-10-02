// Same algorithm as dictops.nx.
//
// `BTreeMap`, not `HashMap`: NX dicts iterate in insertion order and that
// order is observable in the printed checksum, so an unordered Rust map
// could not satisfy the output gate. `BTreeMap` is the closest stdlib
// structure with a matching iteration order, and O(log n) lookup against
// NX's O(n) linear scan is the difference this workload exists to show.

use std::collections::BTreeMap;

fn main() {
    let nkeys: usize = 6000;

    let mut d: BTreeMap<i64, i64> = BTreeMap::new();
    let mut i: i64 = 0;
    while i < nkeys as i64 {
        d.insert(i, i * 3 + 1);
        i += 1;
    }
    println!("{}", d.len());

    let mut p: i64 = 0;
    while p < 4 {
        let mut i: i64 = 0;
        while i < nkeys as i64 {
            let v = *d.get(&i).unwrap();
            d.insert(i, v + p);
            i += 1;
        }
        p += 1;
    }

    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < nkeys as i64 {
        total += *d.get(&i).unwrap();
        i += 1;
    }
    println!("{}", total);

    let mut miss: i64 = 0;
    let mut i: i64 = 0;
    while i < nkeys as i64 {
        if d.contains_key(&(i + nkeys as i64)) {
            miss += 1;
        }
        i += 1;
    }
    println!("{}", miss);

    let mut sumlen: i64 = 0;
    for (_k, v) in d.iter() {
        sumlen += *v;
    }
    println!("{}", sumlen);

    let mut i: i64 = 0;
    while i < nkeys as i64 {
        if i % 2 == 0 {
            d.remove(&i);
        }
        i += 1;
    }
    println!("{}", d.len());

    let mut i: i64 = 0;
    while i < nkeys as i64 {
        d.insert(i, i);
        i += 1;
    }
    println!("{}", d.len());

    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "fox", "golf", "hotel", "india", "juliet",
        "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
        "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "amber", "basalt", "cobalt", "dune",
        "ember", "fjord", "garnet", "halite", "iodine", "jasper", "krypton", "lithium", "magnet",
        "nickel",
    ];

    let nwords: usize = 2400;
    let mut sd: BTreeMap<&str, i64> = BTreeMap::new();
    let mut i: usize = 0;
    while i < nwords {
        sd.insert(words[i % 40], (i % 40) as i64);
        i += 1;
    }
    println!("{}", sd.len());

    let mut hits: i64 = 0;
    let mut i: usize = 0;
    while i < nwords {
        if let Some(v) = sd.get(words[i % 40]) {
            hits += *v;
        }
        i += 1;
    }
    println!("{}", hits);

    let mut wsum: i64 = 0;
    for (k, v) in sd.iter() {
        wsum += (k.len() as i64) * *v;
    }
    println!("{}", wsum);

    let per: usize = 200;
    let mut ml: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    let mut i: i64 = 0;
    while i < per as i64 {
        let mut row: Vec<i64> = Vec::with_capacity(16);
        let mut j: i64 = 0;
        while j < 16 {
            row.push(i * 16 + j);
            j += 1;
        }
        ml.insert(i, row);
        i += 1;
    }
    println!("{}", ml.len());

    let mut rowsum: i64 = 0;
    for (_r, v) in ml.iter() {
        rowsum += v[0] + v[15];
    }
    println!("{}", rowsum);

    let mut lod: Vec<BTreeMap<&str, i64>> = Vec::with_capacity(per);
    let mut i: usize = 0;
    while i < per {
        let mut cell: BTreeMap<&str, i64> = BTreeMap::new();
        cell.insert("row", i as i64);
        cell.insert("flag", (i % 3) as i64);
        lod.push(cell);
        i += 1;
    }
    let mut flags: i64 = 0;
    for c in lod.iter() {
        flags += c["flag"] * c["row"];
    }
    println!("{}", flags);
}