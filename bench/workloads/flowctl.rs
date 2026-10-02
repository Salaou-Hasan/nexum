// Same algorithm as flowctl.nx: n-queens for sizes 8..=11 by backtracking,
// a 0/1 knapsack over a downwards inner loop, and a Collatz window whose
// trip count comes from the data.
//
// The queen solver here takes `&mut Vec<i8>` rather than a fresh vector per
// node, so NX pays a list copy per node for its value semantics and this
// side does not. That difference is the point of the row: it prices
// copy-on-bind rather than hiding it.

fn safe(cols: &[i8], r: i8, c: i8) -> bool {
    let mut k: i8 = 0;
    while (k as usize) < (r as usize) {
        let d = cols[k as usize];
        if d == c {
            return false;
        }
        if (d - k) == (c - r) || (d + k) == (c + r) {
            return false;
        }
        k += 1;
    }
    true
}

fn place(cols: &mut Vec<i8>, r: i8, n: i8) -> i64 {
    if r == n {
        return 1;
    }
    let mut total: i64 = 0;
    let mut c: i8 = 0;
    while c < n {
        if safe(cols, r, c) {
            cols[r as usize] = c;
            total += place(cols, r + 1, n);
        }
        c += 1;
    }
    total
}

fn main() {
    let mut qtotal: i64 = 0;
    let mut qnodes: i64 = 0;
    let mut size: i8 = 8;
    while size <= 11 {
        let mut cols: Vec<i8> = vec![-1i8; size as usize];
        let got = place(&mut cols, 0, size);
        qtotal += got;
        qnodes += 1;
        size += 1;
    }
    println!("{} {}", qtotal, qnodes);

    let cap: usize = 20000;
    let mut iw: Vec<usize> = Vec::with_capacity(48);
    let mut iv: Vec<i64> = Vec::with_capacity(48);
    let mut i: usize = 0;
    while i < 48 {
        iw.push((i * 37 + 11) % 900 + 100);
        iv.push(((i * 613 + 29) % 4000 + 50) as i64);
        i += 1;
    }

    let mut best: Vec<i64> = vec![0i64; cap + 1];

    let mut i: usize = 0;
    while i < iw.len() {
        let w = iw[i];
        let v = iv[i];
        let mut c: i64 = cap as i64;
        while c >= w as i64 {
            let cand = best[(c - w as i64) as usize] + v;
            if cand > best[c as usize] {
                best[c as usize] = cand;
            }
            c -= 1;
        }
        i += 1;
    }
    println!("{} {} {}", best[cap], best[cap / 3], best[0]);

    let mut total: i64 = 0;
    let mut longest: i64 = 0;
    let mut i: i64 = 600000;
    while i < 700000 {
        let mut n = i;
        let mut steps: i64 = 0;
        while n != 1 {
            if n % 2 == 0 {
                n = n / 2;
            } else {
                n = 3 * n + 1;
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