//! 이미지 config 조립: 바탕의 OCI image config에서 시작해 단계마다 고친다.
//!
//! - 바탕의 문서는 JSON 그대로 들고 다닌다. 이 크레이트가 모르는 필드(`ArgsEscaped`, `variant` 등)도 남는다.
//!   OCI 자료형은 어댑터(`sbx-oci`)의 것이므로 여기서는 쓰는 필드만 이름으로 다룬다.
//! - 바탕 config에 `OnBuild`가 있으면 거부한다. 그 명령을 돌리지 않고 이미지를 만들면 Docker와 다른 이미지가 된다.
//! - `ENTRYPOINT`는 같은 스테이지에서 앞서 `CMD`가 없었으면 물려받은 `Cmd`를 지운다(Docker 29로 확인).
//! - history는 BuildKit의 모양을 따른다(Docker 29로 확인): 층을 만드는 단계는 `RUN /bin/sh -c ... # buildkit`,
//!   `COPY ... # buildkit`, `WORKDIR /w`이고, config만 바꾸는 단계는 `empty_layer: true`다. 모든 항목의 `comment`는
//!   `buildkit.dockerfile.v0`이다. BuildKit과 다른 점: `ARG`는 계획에 단계로 남지 않아 history에 없다.
//! - 시각: 층의 시각은 그 층을 만든 때(캐시에서 온 층은 캐시 기록의 시각)이고, config만 바꾸는 단계는 바로 앞 항목의
//!   시각이다. 이미지의 `created`는 마지막 history의 시각이다. 그래서 모두 캐시에서 온 빌드는 같은 config, 같은 이미지
//!   ID다.

use serde_json::{Map, Value, json};

use sbx_model::build::{DEFAULT_PATH, Healthcheck};
use sbx_model::{Arch, ImageConfig};

/// history의 `comment`(BuildKit과 같다).
pub const HISTORY_COMMENT: &str = "buildkit.dockerfile.v0";

/// 만드는 중인 이미지의 config.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// 맨 위 문서(`architecture`, `os` 등). `config`, `rootfs`, `history`, `created`는 따로 둔다.
    doc: Map<String, Value>,
    /// `config`.
    config: Map<String, Value>,
    /// `history`.
    history: Vec<Value>,
    /// 마지막 history의 시각.
    created: String,
}

/// 바탕 config를 읽지 못했다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn string_array(v: &[String]) -> Value {
    Value::Array(v.iter().map(|s| Value::String(s.clone())).collect())
}

impl Image {
    /// `FROM scratch`: `PATH`만 있는 config(BuildKit과 같다).
    pub fn scratch(arch: Arch) -> Self {
        let mut doc = Map::new();
        doc.insert("architecture".into(), arch.oci_name().into());
        doc.insert("os".into(), "linux".into());
        if arch == Arch::Aarch64 {
            doc.insert("variant".into(), "v8".into());
        }
        let mut config = Map::new();
        config.insert("Env".into(), json!([format!("PATH={DEFAULT_PATH}")]));
        config.insert("WorkingDir".into(), "/".into());
        Self {
            doc,
            config,
            history: Vec::new(),
            created: crate::time::rfc3339(0),
        }
    }

    /// 바탕 이미지의 config 문서에서 시작한다. `OnBuild`가 있으면 오류다.
    pub fn from_document(bytes: &[u8]) -> Result<Self, ConfigError> {
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| ConfigError(format!("the image config is not JSON: {e}")))?;
        let Value::Object(mut doc) = v else {
            return Err(ConfigError("the image config is not a JSON object".into()));
        };
        let config = match doc.remove("config") {
            Some(Value::Object(c)) => c,
            None | Some(Value::Null) => Map::new(),
            Some(_) => {
                return Err(ConfigError(
                    "the image config's config is not an object".into(),
                ));
            }
        };
        if !strings(config.get("OnBuild")).is_empty() {
            return Err(ConfigError(
                "the image has ONBUILD instructions, which sbx build does not run yet".into(),
            ));
        }
        let history = match doc.remove("history") {
            Some(Value::Array(h)) => h,
            _ => Vec::new(),
        };
        doc.remove("rootfs");
        let created = match doc.remove("created") {
            Some(Value::String(s)) => s,
            _ => crate::time::rfc3339(0),
        };
        Ok(Self {
            doc,
            config,
            history,
            created,
        })
    }

    /// config 문서가 없는 바탕(0.2.1 전에 받은 이미지): 이미지 기록의 실행 기본값에서 시작한다.
    pub fn from_record(arch: Arch, c: &ImageConfig) -> Self {
        let mut image = Self::scratch(arch);
        image.config = Map::new();
        let env: Vec<String> = c.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        image.config.insert("Env".into(), string_array(&env));
        if !c.entrypoint.is_empty() {
            image
                .config
                .insert("Entrypoint".into(), string_array(&c.entrypoint));
        }
        if !c.cmd.is_empty() {
            image.config.insert("Cmd".into(), string_array(&c.cmd));
        }
        for (k, v) in [
            ("WorkingDir", &c.working_dir),
            ("User", &c.user),
            ("StopSignal", &c.stop_signal),
        ] {
            if let Some(v) = v {
                image.config.insert(k.into(), v.clone().into());
            }
        }
        if !c.labels.is_empty() {
            image.config.insert("Labels".into(), json!(c.labels));
        }
        let set =
            |keys: &[String]| Value::Object(keys.iter().map(|k| (k.clone(), json!({}))).collect());
        if !c.exposed_ports.is_empty() {
            image
                .config
                .insert("ExposedPorts".into(), set(&c.exposed_ports));
        }
        if !c.volumes.is_empty() {
            image.config.insert("Volumes".into(), set(&c.volumes));
        }
        image
    }

    /// `Env`. 순서를 지킨다.
    pub fn env(&self) -> Vec<(String, String)> {
        strings(self.config.get("Env"))
            .into_iter()
            .map(|e| match e.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (e, String::new()),
            })
            .collect()
    }

    /// `ENV`: 같은 이름은 그 자리에서 바꾸고, 새 이름은 끝에 더한다.
    pub fn set_env(&mut self, pairs: &[(String, String)]) {
        let mut env = self.env();
        for (k, v) in pairs {
            match env.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = v.clone(),
                None => env.push((k.clone(), v.clone())),
            }
        }
        let env: Vec<String> = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.config.insert("Env".into(), string_array(&env));
    }

    fn string(&self, key: &str) -> String {
        self.config
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// `WorkingDir`. 없으면 `/`.
    pub fn working_dir(&self) -> String {
        let w = self.string("WorkingDir");
        if w.is_empty() { "/".into() } else { w }
    }

    /// `User`. 없으면 빈 문자열(root).
    pub fn user(&self) -> String {
        self.string("User")
    }

    /// 문자열 필드 하나를 쓴다.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        self.config.insert(key.into(), value.into());
    }

    /// `Entrypoint`. `clear_cmd`면 `Cmd`도 지운다.
    pub fn set_entrypoint(&mut self, argv: &[String], clear_cmd: bool) {
        self.config.insert("Entrypoint".into(), string_array(argv));
        if clear_cmd {
            self.config.insert("Cmd".into(), Value::Null);
        }
    }

    /// `Cmd`.
    pub fn set_cmd(&mut self, argv: &[String]) {
        self.config.insert("Cmd".into(), string_array(argv));
    }

    /// `Shell`(Docker 확장).
    pub fn set_shell(&mut self, shell: &[String]) {
        self.config.insert("Shell".into(), string_array(shell));
    }

    /// `Labels`에 더한다.
    pub fn add_labels(&mut self, pairs: &[(String, String)]) {
        let labels = self
            .config
            .entry("Labels")
            .or_insert_with(|| Value::Object(Map::new()));
        if !labels.is_object() {
            *labels = Value::Object(Map::new());
        }
        if let Value::Object(m) = labels {
            for (k, v) in pairs {
                m.insert(k.clone(), v.clone().into());
            }
        }
    }

    /// `ExposedPorts`나 `Volumes`에 더한다.
    pub fn add_keys(&mut self, field: &str, keys: &[String]) {
        let set = self
            .config
            .entry(field)
            .or_insert_with(|| Value::Object(Map::new()));
        if !set.is_object() {
            *set = Value::Object(Map::new());
        }
        if let Value::Object(m) = set {
            for k in keys {
                m.insert(k.clone(), json!({}));
            }
        }
    }

    /// `Healthcheck`. 시간이 없는 것은 바탕의 값을 물려받는다(Docker와 같다).
    pub fn set_healthcheck(&mut self, h: &Healthcheck) {
        let old = match self.config.get("Healthcheck") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let mut m = Map::new();
        m.insert("Test".into(), string_array(&h.test));
        for (k, v) in [
            ("Interval", h.interval_ns),
            ("Timeout", h.timeout_ns),
            ("StartPeriod", h.start_period_ns),
            ("StartInterval", h.start_interval_ns),
        ] {
            match v {
                Some(v) => {
                    m.insert(k.into(), v.into());
                }
                None => {
                    if let Some(o) = old.get(k) {
                        m.insert(k.into(), o.clone());
                    }
                }
            }
        }
        match h.retries {
            Some(r) => {
                m.insert("Retries".into(), r.into());
            }
            None => {
                if let Some(o) = old.get("Retries") {
                    m.insert("Retries".into(), o.clone());
                }
            }
        }
        self.config.insert("Healthcheck".into(), Value::Object(m));
    }

    /// `MAINTAINER`: 맨 위 `author`.
    pub fn set_author(&mut self, author: &str) {
        self.doc.insert("author".into(), author.into());
    }

    /// 마지막 history의 시각.
    pub fn created(&self) -> &str {
        &self.created
    }

    /// 이미지의 `created`를 정한다(`SOURCE_DATE_EPOCH`).
    pub fn set_created(&mut self, created: &str) {
        self.created = created.to_owned();
    }

    /// history 한 줄을 더한다. `created`가 없으면(config만 바꾸는 단계) 앞 항목의 시각이다.
    pub fn record(&mut self, created_by: &str, created: Option<&str>) {
        self.record_layer(created_by, created, created.is_none());
    }

    /// [`record`](Self::record)와 같고, 층이 없는 단계인지(`empty_layer`)를 따로 준다.
    pub fn record_layer(&mut self, created_by: &str, created: Option<&str>, empty: bool) {
        if let Some(c) = created {
            self.created = c.to_owned();
        }
        let mut h = Map::new();
        h.insert("created".into(), self.created.clone().into());
        h.insert("created_by".into(), created_by.into());
        h.insert("comment".into(), HISTORY_COMMENT.into());
        if empty {
            h.insert("empty_layer".into(), true.into());
        }
        self.history.push(Value::Object(h));
    }

    /// config 문서(OCI image config JSON). 키 순서가 정해져 있어 같은 내용이면 같은 바이트다.
    pub fn document(&self, diff_ids: &[String]) -> Vec<u8> {
        let mut doc = self.doc.clone();
        doc.insert("created".into(), self.created.clone().into());
        doc.insert("config".into(), Value::Object(self.config.clone()));
        doc.insert(
            "rootfs".into(),
            json!({ "type": "layers", "diff_ids": diff_ids }),
        );
        doc.insert("history".into(), Value::Array(self.history.clone()));
        serde_json::to_vec(&Value::Object(doc)).expect("a JSON value serializes")
    }

    /// 이미지 기록에 쓰는 실행 기본값.
    pub fn image_config(&self) -> ImageConfig {
        let keys = |field: &str| -> Vec<String> {
            match self.config.get(field) {
                Some(Value::Object(m)) => m.keys().cloned().collect(),
                _ => Vec::new(),
            }
        };
        let opt = |field: &str| Some(self.string(field)).filter(|s| !s.is_empty());
        ImageConfig {
            entrypoint: strings(self.config.get("Entrypoint")),
            cmd: strings(self.config.get("Cmd")),
            env: self.env(),
            working_dir: opt("WorkingDir"),
            user: opt("User"),
            stop_signal: opt("StopSignal"),
            labels: match self.config.get("Labels") {
                Some(Value::Object(m)) => m
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect(),
                _ => Default::default(),
            },
            exposed_ports: keys("ExposedPorts"),
            volumes: keys("Volumes"),
        }
    }
}

/// Go의 `%q`로 문자열 목록을 쓴 모양(`["a" "b"]`). BuildKit의 history가 이렇게 쓴다.
pub fn go_quoted(v: &[String]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|s| serde_json::to_string(s).expect("a string serializes"))
        .collect();
    format!("[{}]", items.join(" "))
}

/// Go의 `%v`로 문자열 목록을 쓴 모양(`[a b]`).
pub fn go_plain(v: &[String]) -> String {
    format!("[{}]", v.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Image {
        Image::from_document(
            br#"{"architecture":"arm64","os":"linux","created":"2024-01-01T00:00:00Z",
                "config":{"Env":["PATH=/bin","A=1"],"Cmd":["sh"],"ArgsEscaped":true},
                "rootfs":{"type":"layers","diff_ids":["sha256:aa"]},
                "history":[{"created":"2024-01-01T00:00:00Z","created_by":"x"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn env_replaces_in_place_and_appends_new_names() {
        let mut i = base();
        i.set_env(&[("A".into(), "2".into()), ("B".into(), "3".into())]);
        assert_eq!(
            i.env(),
            [("PATH", "/bin"), ("A", "2"), ("B", "3")].map(|(k, v)| (k.into(), v.into()))
        );
    }

    #[test]
    fn entrypoint_clears_an_inherited_cmd() {
        let mut i = base();
        i.set_entrypoint(&["/app".into()], true);
        assert!(i.image_config().cmd.is_empty());
        let mut i = base();
        i.set_cmd(&["x".into()]);
        i.set_entrypoint(&["/app".into()], false);
        assert_eq!(i.image_config().cmd, ["x"]);
    }

    #[test]
    fn the_document_keeps_unknown_fields_and_rewrites_rootfs_and_history() {
        let mut i = base();
        i.record("RUN x # buildkit", Some("2025-01-01T00:00:00Z"));
        i.record("ENV K=v", None);
        let doc: Value = serde_json::from_slice(&i.document(&["sha256:aa".into()])).unwrap();
        assert_eq!(doc["config"]["ArgsEscaped"], true);
        assert_eq!(doc["created"], "2025-01-01T00:00:00Z");
        assert_eq!(doc["rootfs"]["diff_ids"], json!(["sha256:aa"]));
        let h = doc["history"].as_array().unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[2]["empty_layer"], true);
        assert_eq!(h[2]["created"], "2025-01-01T00:00:00Z");
        assert_eq!(h[1]["comment"], HISTORY_COMMENT);
        // 같은 내용이면 같은 바이트다.
        assert_eq!(i.document(&[]), i.clone().document(&[]));
    }

    #[test]
    fn onbuild_bases_are_refused() {
        let e = Image::from_document(br#"{"config":{"OnBuild":["RUN x"]}}"#).unwrap_err();
        assert!(e.0.contains("ONBUILD"), "{e:?}");
        assert!(Image::from_document(br#"{"config":{"OnBuild":null}}"#).is_ok());
    }

    #[test]
    fn healthcheck_keeps_the_base_timings_it_does_not_set() {
        let mut i = base();
        i.set_healthcheck(&Healthcheck {
            test: vec!["CMD".into(), "true".into()],
            interval_ns: Some(5),
            timeout_ns: None,
            start_period_ns: None,
            start_interval_ns: None,
            retries: Some(2),
        });
        i.set_healthcheck(&Healthcheck {
            test: vec!["NONE".into()],
            interval_ns: None,
            timeout_ns: None,
            start_period_ns: None,
            start_interval_ns: None,
            retries: None,
        });
        let doc: Value = serde_json::from_slice(&i.document(&[])).unwrap();
        assert_eq!(
            doc["config"]["Healthcheck"],
            json!({"Test":["NONE"],"Interval":5,"Retries":2})
        );
    }

    #[test]
    fn scratch_starts_with_the_default_path() {
        let i = Image::scratch(Arch::X86_64);
        assert_eq!(i.env(), [("PATH".into(), DEFAULT_PATH.into())]);
        assert_eq!(i.working_dir(), "/");
        assert_eq!(go_quoted(&["a".into(), "b c".into()]), r#"["a" "b c"]"#);
        assert_eq!(go_plain(&["80/tcp".into()]), "[80/tcp]");
    }
}
