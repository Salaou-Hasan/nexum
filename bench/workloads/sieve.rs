// Same algorithm as sieve.nx: a sieve of Eratosthenes over a flag table,
// a totient accumulation, and a branchy integer kernel over the surviving
// primes. Every value is i64 on both sides and every intermediate stays
// well inside i64, so the printed integers must match exactly.

fn main() {
    let limit: i64 = 2500000;

    let mut sieve: Vec<i64> = Vec::with_capacity(limit as usize + 1);
    let mut i: i64 = 0;
    while i <= limit {
        sieve.push(1);
        i += 1;
    }
    sieve[0] = 0;
    sieve[1] = 0;

    let mut marked: i64 = 0;
    let mut p: i64 = 2;
    while p * p <= limit {
        if sieve[p as usize] == 1 {
            let mut m = p * p;
            while m <= limit {
                sieve[m as usize] = 0;
                marked += 1;
                m += p;
            }
        }
        p += 1;
    }

    let mut nprimes: i64 = 0;
    let mut psum: i64 = 0;
    let mut i: i64 = 0;
    while i <= limit {
        if sieve[i as usize] == 1 {
            nprimes += 1;
            psum += i;
        }
        i += 1;
    }
    println!("{} {} {}", nprimes, psum, marked);

    let mut phi: Vec<i64> = (0..=limit).collect();
    let mut p: i64 = 2;
    while p <= limit {
        if phi[p as usize] == p {
            let mut m = p;
            while m <= limit {
                phi[m as usize] = phi[m as usize] - phi[m as usize] / p;
                m += p;
            }
        }
        p += 1;
    }

    let mut tsum: i64 = 0;
    let mut i: i64 = 0;
    while i <= limit {
        tsum += phi[i as usize];
        i += 1;
    }
    println!("{} {}", nprimes, tsum);

    let mut h: i64 = 0;
    let mut state: i64 = 12345;
    let mut p: i64 = 2;
    while p <= limit {
        if sieve[p as usize] == 1 {
            state = (state * 1103515245 + 12345) % 2147483648;
            let b = state % 97;
            if b > 48 {
                h = (h + p * b + ((p >> 3) ^ (state & 65535))) % 1000000007;
            } else if b > 24 {
                h = (h + 1000000007 - p * (b + 1)) % 1000000007;
            } else {
                h = (h + p) % 1000000007;
            }
        }
        p += 1;
    }
    println!("{}", h);
    println!("{} {}", sieve.len(), phi.len());
}