// Same algorithm as flowctl.nx: n-queens for sizes 9..=11 by backtracking
// over three bitmasks, a 0/1 knapsack with a downwards inner loop, and a
// Collatz window whose trip count comes from the data.
//
// All five arguments to `place` are i64, so NX memoizes it here as well
// and this side does not. That difference is called out in the .nx header
// rather than engineered away.

fn ok(cmask: i64, d1: i64, d2: i64, c: i64) -> bool {
    let b = 1i64 << c;
    if (cmask & b) != 0 {
        return false;
    }
    if (d1 & b) != 0 {
        return false;
    }
    if (d2 & b) != 0 {
        return false;
    }
    true
}

fn place(cmask: i64, d1: i64, d2: i64, left: i64, nn: i64) -> i64 {
    if left == 0 {
        return 1;
    }
    let mut total: i64 = 0;
    let mut c: i64 = 0;
    while c < nn {
        if ok(cmask, d1, d2, c) {
            total += place(
                cmask | (1i64 << c),
                (d1 | (1i64 << c)) << 1,
                (d2 | (1i64 << c)) >> 1,
                left - 1,
                nn,
            );
        }
        c += 1;
    }
    total
}

fn main() {
    let mut qtotal: i64 = 0;
    let mut qsizes: i64 = 0;
    let mut qn: i64 = 9;
    while qn <= 11 {
        qtotal += place(0, 0, 0, qn, qn);
        qsizes += 1;
        qn += 1;
    }
    println!("{} {}", qtotal, qsizes);

    let cap: i64 = 20000;
    let mut iw: Vec<i64> = Vec::with_capacity(48);
    let mut iv: Vec<i64> = Vec::with_capacity(48);
    let mut i: i64 = 0;
    while i < 48 {
        iw.push((i * 37 + 11) % 900 + 100);
        iv.push((i * 613 + 29) % 4000 + 50);
        i += 1;
    }

    let mut best: Vec<i64> = vec![0i64; (cap + 1) as usize];

    let mut i: usize = 0;
    while i < iw.len() {
        let w = iw[i];
        let v = iv[i];
        let mut c: i64 = cap;
        while c >= w {
            let cand = best[(c - w) as usize] + v;
            if cand > best[c as usize] {
                best[c as usize] = cand;
            }
            c -= 1;
        }
        i += 1;
    }
    println!(
        "{} {} {}",
        best[cap as usize],
        best[(cap / 3) as usize],
        best[0]
    );

    let mut total: i64 = 0;
    let mut longest: i64 = 0;
    let mut i: i64 = 600000;
    while i < 700000 {
        let mut v = i;
        let mut steps: i64 = 0;
        while v != 1 {
            if v % 2 == 0 {
                v = v / 2;
            } else {
                v = 3 * v + 1;
            }
            steps += 1;
        }
        if steps > longest {
            longest = steps;
        }
        total += steps;
        i += 1;
    }
    println!("{} {}", total, longest);
}