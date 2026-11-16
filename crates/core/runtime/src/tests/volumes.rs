//! 이름 붙은 볼륨을 fake 실행기 위에서 돌린다. fake는 볼륨 디스크를 디스크 옆 디렉터리(`volume_content`)로 흉내 낸다.

use super::*;

fn mount(name: &str, dst: &Path, readonly: bool) -> VolumeMount {
    VolumeMount {
        name: name.into(),
        destination: dst.display().to_string(),
        readonly,
        no_copy: false,
    }
}

/// 볼륨 하나를 붙여 `args`를 돌리는 샌드박스를 만든다.
fn with_volumes(e: &Env, name: Option<&str>, vols: Vec<VolumeMount>, args: &[&str]) -> Sandbox {
    e.prepare_image();
    let mut spec = e.spec(args);
    spec.name = name.map(Into::into);
    spec.volumes = vols;
    block_on(e.rt.create(spec)).unwrap()
}

/// 볼륨 디스크는 층 디스크와 scratch 뒤에 역할 `Volume`으로 붙고, 층으로 세지 않는다(`CheckLayers`가 지나간다).
/// 쓴 내용은 샌드박스를 지워도 남아 다음 샌드박스가 본다.
#[test]
fn volume_disks_come_last_and_outlive_the_sandbox() {
    let e = Env::new("vol-order");
    let v = e.rt.create_volume("data1", 64, BTreeMap::new()).unwrap();
    let target = e.root.join("g/data");
    let s = with_volumes(
        &e,
        None,
        vec![mount("data1", &target, false)],
        &["sh", "-c", &format!("echo kept > {}/f", target.display())],
    );
    let (_, err, code) = collect(block_on(s.start(IoMode::default())).unwrap());
    assert_eq!(code, Some(0), "{err}");
    let launch = &e.fake.launches()[0];
    let roles: Vec<DiskRole> = launch.disks.iter().map(|d| d.role).collect();
    assert_eq!(
        roles,
        [DiskRole::Rootfs, DiskRole::Scratch, DiskRole::Volume]
    );
    assert_eq!(launch.disks[2].path, v.disk());
    assert!(!launch.disks[2].readonly);
    e.wait_state(&s, State::Stopped);
    block_on(s.delete()).unwrap();
    assert!(e.rt.volume("data1").unwrap().is_some());

    let s = with_volumes(
        &e,
        None,
        vec![mount("data1", &target, true)],
        &["cat", &format!("{}/f", target.display())],
    );
    let (out, err, code) = collect(block_on(s.start(IoMode::default())).unwrap());
    assert_eq!((out.as_str(), code), ("kept\n", Some(0)), "{err}");
    assert!(e.fake.launches()[1].disks[2].readonly);
}

/// 쓰기로 붙인 볼륨은 실행 중인 샌드박스 하나만 쓴다. 두 번째는 VM을 띄우기 전에 쓰는 쪽의 이름과 함께 거부되고,
/// 첫째가 멈추면 된다. 읽기 전용끼리는 함께 붙는다.
#[test]
fn one_writer_at_a_time_and_readers_share() {
    let e = Env::new("vol-lock");
    e.rt.create_volume("pg", 64, BTreeMap::new()).unwrap();
    let target = e.root.join("g/pg");
    let db = with_volumes(
        &e,
        Some("db"),
        vec![mount("pg", &target, false)],
        &["sleep", "30"],
    );
    let _main = block_on(db.start(IoMode::default())).unwrap();
    let other = with_volumes(&e, None, vec![mount("pg", &target, false)], &["true"]);
    let launches = e.fake.launches().len();
    match block_on(other.start(IoMode::default())) {
        Err(RuntimeError::VolumeBusy { name, holder }) => {
            assert_eq!((name.as_str(), holder.as_deref()), ("pg", Some("db")));
        }
        r => panic!("{:?}", r.map(|_| ())),
    }
    assert_eq!(
        e.fake.launches().len(),
        launches,
        "no VM for a refused start"
    );
    let reader = with_volumes(&e, None, vec![mount("pg", &target, true)], &["true"]);
    assert!(matches!(
        block_on(reader.start(IoMode::default())),
        Err(RuntimeError::VolumeBusy { .. })
    ));
    // 지우기도 거부한다(샌드박스 기록이 가리킨다).
    let err = block_on(e.rt.remove_volume("pg")).unwrap_err().to_string();
    assert!(err.contains("in use by sandboxes"), "{err}");
    block_on(db.stop(None, Duration::from_secs(5))).unwrap();
    e.wait_state(&db, State::Stopped);

    // 읽기 전용 둘은 함께 붙는다.
    let r1 = with_volumes(&e, None, vec![mount("pg", &target, true)], &["sleep", "30"]);
    let r2 = with_volumes(
        &e,
        None,
        vec![mount("pg", &e.root.join("g/pg2"), true)],
        &["sleep", "30"],
    );
    let _a = block_on(r1.start(IoMode::default())).unwrap();
    let _b = block_on(r2.start(IoMode::default())).unwrap();
    // 읽는 쪽이 있으면 쓰는 쪽은 붙지 못한다.
    assert!(matches!(
        block_on(other.start(IoMode::default())),
        Err(RuntimeError::VolumeBusy { .. })
    ));
    block_on(r1.stop(None, Duration::from_secs(5))).unwrap();
    block_on(r2.stop(None, Duration::from_secs(5))).unwrap();
    e.wait_state(&r2, State::Stopped);
    let (_, err, code) = collect(block_on(other.start(IoMode::default())).unwrap());
    assert_eq!(code, Some(0), "{err}");
}

/// 없는 볼륨은 만들지 않는다. 볼륨이 디스크를 쓰는 만큼 층 디스크가 준다. 너무 많으면 만들기가 거부한다.
#[test]
fn missing_volumes_and_the_disk_budget() {
    let e = Env::new("vol-budget");
    e.prepare_image();
    let mut spec = e.spec(&["true"]);
    spec.volumes = vec![mount("nope", &e.root.join("x"), false)];
    assert!(matches!(
        block_on(e.rt.create(spec)),
        Err(RuntimeError::VolumeNotFound(n)) if n == "nope"
    ));
    let max = sbx_executor_fake::MAX_DISKS;
    assert_eq!(e.rt.layer_disks(), max - 1);
    assert_eq!(e.rt.layer_disks_for(2), max - 3);
    let mut spec = e.spec(&["true"]);
    for i in 0..max - 1 {
        let n = format!("v{i}");
        e.rt.create_volume(&n, 64, BTreeMap::new()).unwrap();
        spec.volumes.push(mount(&n, &e.root.join(&n), false));
    }
    let err = block_on(e.rt.create(spec.clone())).unwrap_err();
    assert!(
        matches!(err, RuntimeError::TooManyVolumes { volumes, .. } if volumes == max - 1),
        "{err}"
    );
    spec.volumes.pop();
    block_on(e.rt.create(spec)).unwrap();
}

/// 규약 3을 말하지 않는 실행기는 볼륨을 붙이지 못한다.
#[test]
fn old_executors_cannot_take_volumes() {
    let mut caps = FakeExecutor::basic().capabilities();
    caps.volumes = false;
    let e = Env::with("vol-old", FakeExecutor::new(caps));
    e.rt.create_volume("v1", 64, BTreeMap::new()).unwrap();
    e.prepare_image();
    let mut spec = e.spec(&["true"]);
    spec.volumes = vec![mount("v1", &e.root.join("x"), false)];
    let err = block_on(e.rt.create(spec)).unwrap_err().to_string();
    assert!(err.contains("cannot attach volumes"), "{err}");
}
