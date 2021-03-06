use nanoserde::{DeBin, DeJson, SerBin, SerJson};

fn main() {
    #[derive(DeBin, SerBin, DeJson, SerJson, PartialEq, Debug)]
    pub struct Test {
        pub a: i32,
        pub b: f32,
        c: Option<String>,
        d: Option<String>,
        e: Option<std::collections::HashMap<String, String>>,
    }

    let mut map = std::collections::HashMap::new();
    map.insert("a".to_string(), "b".to_string());

    let test: Test = Test {
        a: 1,
        b: 2.,
        c: Some("asd".to_string()),
        d: None,
        e: Some(map),
    };

    let bytes = SerBin::serialize_bin(&test);
    let test_deserialized = DeBin::deserialize_bin(&bytes).unwrap();
    dbg!(bytes);
    dbg!(test_deserialized);

    let bytes = SerJson::serialize_json(&test);
    let test_deserialized = DeJson::deserialize_json(&bytes).unwrap();
    dbg!(test_deserialized);
    dbg!(test);
}
