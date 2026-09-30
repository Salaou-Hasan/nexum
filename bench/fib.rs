// Same algorithm as fib.nx. Recursive, no memo table: this is the
// baseline NX is compared against when its own memoization is disabled.

fn fib(n: u64) -> u64 {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() {
    println!("{}", fib(32));
}
