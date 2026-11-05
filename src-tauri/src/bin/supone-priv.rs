// 作者: kejiqing
//! 停内核特权 helper：安装到 data/bin 后 setuid root，只杀本壳 mihomo。

fn main() {
    let code = supone_tunnel::privkill_main(std::env::args_os());
    std::process::exit(code);
}
