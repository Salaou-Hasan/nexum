// Same algorithm as recursion.nx.
//
// Phase 3 clones `xs` on every call, because NX's argument passing copies
// the container and this row is about that copy. A Rust programmer would
// write `&[i64]`; cloning is deliberate so the copy count matches and the
// row measures the value representation rather than borrowing against
// copying.
//
// Phase 2 reads the tree from a module-level static rather than passing
// it, for the same reason: passing it would make the NX side clone a
// 524288-element table per node. Rust can borrow, so `walk` takes `&[i64]`
// and the difference between the two shapes is exactly the copy-on-bind
// cost NX cannot avoid.

fn chain(n: i64, acc: i64) -> i64 {
    if n <= 0 {
        return acc;
    }
    chain(n - 1, acc + n)
}

static mut TREE: Vec<i64> = Vec::new();

fn walk(node: usize, b: usize) -> i64 {
    if node >= b {
        return unsafe { TREE[node] };
    }
    walk(node * 2, b) + walk(node * 2 + 1, b)
}

fn sum1(xs: Vec<i64>) -> i64 {
    let mut t: i64 = 0;
    let mut i: usize = 0;
    while i < xs.len() {
        t += xs[i];
        i += 1;
    }
    t
}

fn main() {
    let depth: i64 = 350;
    let rounds: usize = 400000;

    let mut ctotal: i64 = 0;
    let mut r: usize = 0;
    while r < rounds {
        ctotal += chain(depth, 0);
        r += 1;
    }
    println!("{}", ctotal);

    let leaves: usize = 524288;
    let mut base: usize = 1;
    while base < leaves {
        base *= 2;
    }

    let mut tree: Vec<i64> = vec![0i64; 2 * base];
    let mut j: usize = 0;
    while j < base {
        tree[base + j] = ((j as i64) * 2654435761) % 65521;
        j += 1;
    }
    let mut i: usize = base - 1;
    while i > 0 {
        tree[i] = tree[2 * i] + tree[2 * i + 1];
        i -= 1;
    }
    unsafe {
        TREE = tree;
    }

    let wtotal = walk(1, base);
    println!("{} {}", wtotal, base);

    let payload: Vec<i64> = (0..64).map(|i| i as i64 * 3 + 1).collect();

    let mut stotal: i64 = 0;
    let mut r: usize = 0;
    while r < rounds {
        stotal += sum1(payload.clone());
        r += 1;
    }
    println!("{} {}", stotal, payload.len());
}