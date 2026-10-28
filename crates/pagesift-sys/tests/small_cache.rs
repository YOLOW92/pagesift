//! 整个测试程序跑在 SmallCache 上：多线程随机分配、释放、扩缩容，跨线程释放，短命线程。
//! 拓扑是本机的（不调用 tune_allocator 时只有一个分片）。

mod stress;

#[global_allocator]
static ALLOC: pagesift_sys::SmallCache = pagesift_sys::SmallCache;

fn setup() {}
