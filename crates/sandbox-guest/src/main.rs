//! 게스트 init 진입점. 부팅 단계는 아직 없다(로드맵 7b).

fn main() {
    eprintln!(
        "sandbox-guest: boot is not implemented yet (protocol v{})",
        sandbox_proto::PROTOCOL_VERSION
    );
    std::process::exit(1);
}
