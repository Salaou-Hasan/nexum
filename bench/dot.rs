// Same as dot.nx: a read-only dot product over two flat vectors, with the
// fill hoisted out of the timed region by the same loop structure.

fn main() {
    let n: usize = 2000000;
    let mut a = vec![0.0f64; n];
    let mut b = vec![0.0f64; n];
    let mut i = 0usize;
    while i < n {
        a[i] = (i - (i / 3) * 3) as f64;
        b[i] = (i - (i / 5) * 5) as f64;
        i += 1;
    }

    let mut s = 0.0f64;
    let mut i = 0usize;
    while i < n {
        s += a[i] * b[i];
        i += 1;
    }

    println!("{}", s);
    println!("{}", a.len());
}
