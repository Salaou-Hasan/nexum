// Same algorithm as churn.nx.
//
// The one place the two sides differ on purpose: phase 5. NX allocates a
// new string on every `+` and the native runtime never frees one, so the
// NX working set grows without bound while this side's does not. The
// counts match; the memory behaviour does not, and that is the finding.

fn main() {
    let rounds: usize = 400000;

    let mut acc: i64 = 0;
    let mut r: i64 = 0;
    while r < rounds as i64 {
        let mut t: Vec<i64> = Vec::with_capacity(16);
        let mut i: i64 = 0;
        while i < 16 {
            t.push(r * 16 + i);
            i += 1;
        }
        acc += t[0] + t[15] + t.len() as i64;
        r += 1;
    }
    println!("{} {}", acc, rounds);

    let mut bacc: i64 = 0;
    let mut r: i64 = 0;
    while r < (rounds / 4) as i64 {
        let mut d: Vec<(i64, i64)> = Vec::with_capacity(8);
        let mut i: i64 = 0;
        while i < 8 {
            d.push((i, r * 8 + i));
            i += 1;
        }
        bacc += d[0].1 + d[7].1 + d.len() as i64;
        r += 1;
    }
    println!("{}", bacc);

    let mut cacc: i64 = 0;
    let mut r: i64 = 0;
    while r < (rounds / 8) as i64 {
        let mut outer: Vec<Vec<i64>> = Vec::with_capacity(8);
        let mut i: i64 = 0;
        while i < 8 {
            let mut inner: Vec<i64> = Vec::with_capacity(8);
            let mut j: i64 = 0;
            while j < 8 {
                inner.push(i * 8 + j);
                j += 1;
            }
            outer.push(inner);
            i += 1;
        }
        cacc += outer[0][0] + outer[7][7];
        r += 1;
    }
    println!("{}", cacc);

    let mut dacc: i64 = 0;
    let mut r: i64 = 0;
    while r < (rounds / 8) as i64 {
        let mut d: Vec<(i64, i64)> = Vec::with_capacity(32);
        let mut i: i64 = 0;
        while i < 32 {
            d.push((i, i));
            i += 1;
        }
        let mut i: i64 = 0;
        while i < 16 {
            d.remove(0);
            i += 1;
        }
        dacc += d.len() as i64 + d[15].1;
        r += 1;
    }
    println!("{}", dacc);

    let mut sacc: i64 = 0;
    let mut r: i64 = 0;
    while r < 40000 {
        let s = String::from("abcdefghijklmnopqrstuvwxyz") + "0123456789";
        sacc += s.len() as i64;
        r += 1;
    }
    println!("{}", sacc);
}