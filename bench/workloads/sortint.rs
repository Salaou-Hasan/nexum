// Same algorithm as sortint.nx: CLRS quicksort with a median-of-three, an
// explicit (lo, hi) stack, a small-partition cutoff, and a final
// insertion pass. Same comparison and swap counters, same fill.
//
// The recursion and the `swap` helper that a Rust programmer would reach
// for are written iteratively and inlined here on purpose, so that the
// two sides run the same instruction sequence rather than two different
// algorithms.

fn main() {
    let n: usize = 2000000;

    let mut xs: Vec<i64> = Vec::with_capacity(n);
    let mut i: usize = 0;
    while i < n {
        xs.push(((i as i64) * 2654435761) % 4294967296);
        i += 1;
    }

    let mut stack: Vec<usize> = vec![0usize; 64];
    let mut sp: usize = 0;
    stack[sp] = 0;
    sp += 1;
    stack[sp] = n - 1;
    sp += 1;

    let cut: usize = 16;

    let mut cmp: i64 = 0;
    let mut swp: i64 = 0;
    while sp > 0 {
        sp -= 1;
        let hi = stack[sp];
        sp -= 1;
        let lo = stack[sp];

        let mid = lo + (hi - lo) / 2;
        if xs[mid] < xs[lo] {
            xs.swap(mid, lo);
            cmp += 1;
        }
        if xs[hi] < xs[lo] {
            xs.swap(hi, lo);
            cmp += 1;
        }
        if xs[hi] < xs[mid] {
            xs.swap(hi, mid);
            cmp += 1;
        }
        xs.swap(mid, hi - 1);

        let pivot = xs[hi - 1];
        let mut m = lo;
        let mut j = lo;
        while j < hi - 1 {
            if xs[j] <= pivot {
                xs.swap(m, j);
                swp += 1;
                m += 1;
            }
            j += 1;
            cmp += 1;
        }
        xs.swap(m, hi - 1);

        if m - lo > cut {
            stack[sp] = lo;
            sp += 1;
            stack[sp] = m - 1;
            sp += 1;
        }
        if hi - m > cut {
            stack[sp] = m + 1;
            sp += 1;
            stack[sp] = hi;
            sp += 1;
        }
    }

    let mut i: usize = 1;
    while i < n {
        let key = xs[i];
        let mut j = i as i64 - 1;
        while j >= 0 && xs[j as usize] > key {
            xs[(j + 1) as usize] = xs[j as usize];
            j -= 1;
            cmp += 1;
        }
        xs[(j + 1) as usize] = key;
        i += 1;
    }

    let mut bad: i64 = 0;
    let mut chk: i64 = 0;
    let mut i: usize = 0;
    while i < n {
        if i > 0 && xs[i - 1] > xs[i] {
            bad += 1;
        }
        chk = (chk + xs[i] * (i as i64 + 1)) % 1000000007;
        i += 1;
    }

    println!("{} {}", n, bad);
    println!("{} {} {}", xs[0], xs[n - 1], xs[n / 2]);
    println!("{} {} {}", chk, cmp, swp);
}