// Same algorithm as listsum.nx. Vec<i64> is a flat allocation, so this
// is the shape where Rust has the largest structural advantage: NX list
// elements are boxed values reached through a pointer.

fn main() {
    let n: usize = 2000000;
    let mut xs: Vec<i64> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        xs.push(((i as i64 * 31) % 1000) as i64);
        i += 1;
    }

    let mut total: i64 = 0;
    let mut i = 0usize;
    while i < n {
        total += xs[i];
        i += 1;
    }

    let mut rev: i64 = 0;
    let mut i = n - 1;
    loop {
        rev += xs[i] * 2;
        if i == 0 {
            break;
        }
        i -= 1;
    }

    println!("{}", total);
    println!("{}", rev);
    println!("{}", xs.len());
}
