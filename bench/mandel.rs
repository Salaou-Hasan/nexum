// Same algorithm as mandel.nx. Floats are f64 throughout, matching NX's
// double representation exactly, so the iteration counts must agree.

fn mandel(cx: f64, cy: f64, maxiter: i64) -> i64 {
    let mut zr = 0.0f64;
    let mut zi = 0.0f64;
    let mut i = 0i64;
    while i < maxiter {
        let zr2 = zr * zr;
        let zi2 = zi * zi;
        if zr2 + zi2 > 4.0 {
            return i;
        }
        zi = 2.0 * zr * zi + cy;
        zr = zr2 - zi2 + cx;
        i += 1;
    }
    maxiter
}

fn main() {
    let mut total = 0i64;
    let mut y = -1.2f64;
    let mut rows = 0i64;
    while y < 1.2 {
        let mut x = -1.5f64;
        while x < 1.5 {
            total += mandel(x, y, 500);
            x += 0.002;
        }
        y += 0.002;
        rows += 1;
    }

    println!("{}", total);
    println!("{}", rows);
}
