// Same algorithm as matmul.nx. Row-major flat vectors, identical fill,
// identical accumulation order, so the printed values must match exactly.

fn main() {
    let n: usize = 120;
    let mut a = vec![0.0f64; n * n];
    let mut b = vec![0.0f64; n * n];
    let mut c = vec![0.0f64; n * n];

    let mut i = 0usize;
    while i < n * n {
        a[i] = ((i as i64 * 7919) % 1000) as f64 * 0.5 - 250.0;
        b[i] = ((i as i64 * 6271) % 997) as f64 * 0.25 - 124.0;
        i += 1;
    }

    let mut i = 0usize;
    while i < n {
        let mut j = 0usize;
        while j < n {
            let mut s = 0.0f64;
            let mut k = 0usize;
            while k < n {
                s += a[i * n + k] * b[k * n + j];
                k += 1;
            }
            c[i * n + j] = s;
            j += 1;
        }
        i += 1;
    }

    let mut check = 0.0f64;
    let mut i = 0usize;
    while i < n * n {
        check += c[i] * (1.0 + (i % 7) as f64);
        i += 1;
    }

    println!("{}", check);
    println!("{}", c[0]);
    println!("{}", c[n * n - 1]);
}
