// Same work as parwork.nx, run sequentially on one thread.
//
// `parallel:` in NX runs conflict-free tasks on a thread pool and
// serialises conflicting ones. This reference is the sequential equivalent
// of both halves, so a `parallel:` half that runs in about half the time of
// the sequential one is evidence the pool engaged, and a half that runs in
// about the same time is evidence it did not.

const BURN: i64 = 150000000;

fn burn_it(k: i64) -> i64 {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < BURN {
        t += (i * k) % 1009;
        i += 1;
    }
    t
}

fn main() {
    let a = burn_it(1);
    let b = burn_it(2);
    println!("{} {}", a, b);

    // The control: both tasks write `c`, so NX serialises them in program
    // order and the second value wins. The first call still runs.
    let _discarded = burn_it(3);
    let c = burn_it(4);
    println!("{}", c);
}