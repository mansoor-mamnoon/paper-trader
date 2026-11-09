//! Byte-level regressions through the public exporters and real SQLite databases.

use paper_trader::{
    export, ledger,
    telemetry::{RunMeta, Telemetry},
};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

fn directory(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/e1760dd857154a71a1d00337db45a8b2")
        .join(format!("{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn dashboard_csv_preserves_sql_types_and_escaping() {
    let dir = directory("dashboard");
    let path = dir.join("ledger.sqlite");
    let db = ledger::connect(path.to_str().unwrap()).unwrap();
    db.execute_batch(
        "INSERT INTO events(id,at,kind,message) VALUES
         (4,1e999,'infinity',''),
         (2,2.5,NULL,X'0102'),
         (1,2.0,'a,b','say \"hi\"'),
         (3,-1e999,CAST(X'FF' AS TEXT),'line' || char(10) || 'return' || char(13));
         INSERT INTO decisions(id,at,symbol,action,reason,evidence)
         VALUES(1,NULL,'X','hold','','{\"ok\":true,\"value\":null}');",
    )
    .unwrap();
    let out = dir.join("site");
    export::export(path.to_str().unwrap(), out.to_str().unwrap(), None).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("export/events.csv")).unwrap(),
        "id,at,kind,message\n1,2.0,\"a,b\",\"say \"\"hi\"\"\"\n2,2.5,,\n3,-inf,�,\"line\nreturn\r\"\n4,inf,infinity,\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("export/decisions.csv")).unwrap(),
        "id,at,symbol,action,reason,evidence\n1,,X,hold,,\"{\"\"ok\"\":true,\"\"value\"\":null}\"\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("export/fills.csv")).unwrap(),
        "id,at,signal_at,symbol,side,quantity,price,notional,fee,realized_pnl,reason\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("export/equity.csv")).unwrap(),
        "id,at,equity,cash,benchmark,drawdown,planned_risk\n"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn telemetry_csv_preserves_json_number_and_null_semantics() {
    let dir = directory("telemetry");
    let path = dir.join("telemetry.sqlite");
    let telemetry =
        Telemetry::open(path.to_str().unwrap(), "run", &RunMeta::default(), 2.0).unwrap();
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "INSERT INTO events(id,run_id,at,kind,subject,detail) VALUES
         (4,'run',1e999,'infinity','',NULL),
         (2,'run',2.5,NULL,X'0102',''),
         (1,'run',2.0,'a,b','say \"hi\"','line' || char(10) || 'return' || char(13)),
         (3,'run',-1e999,CAST(X'FF' AS TEXT),'true','false');
         INSERT INTO fetches(id,run_id,at,source,ok,ms) VALUES
         (1,'run',2.0,'source',1,10),(2,'run',2.0,'source',0,20);",
    )
    .unwrap();
    let out = dir.join("csv");
    telemetry.export_csv(&out).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("events.csv")).unwrap(),
        "id,run_id,at,kind,subject,detail\n1,run,2.0,\"a,b\",\"say \"\"hi\"\"\",\"line\nreturn\r\"\n2,run,2.5,,,\n3,run,,�,true,false\n4,run,,infinity,,\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("fetches.csv")).unwrap(),
        "id,run_id,at,source,ok,ms\n1,run,2.0,source,1,10\n2,run,2.0,source,0,20\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("stages.csv")).unwrap(),
        "id,run_id,name,started_at,ms,ok,note\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("llm_calls.csv")).unwrap(),
        "id,run_id,at,stage,role,model_id,company,prompt_chars,prompt_hash,output_chars,output_hash,latency_ms,prompt_tokens,completion_tokens,prompt_tps,gen_tps,ok,error\n"
    );
    assert_eq!(
        fs::read_to_string(out.join("runs.csv")).unwrap(),
        "run_id,started_at,ended_at,trigger,git_sha,cpu,cores,ram_mb,desk_minutes,status,minutes_used,minutes_budget,degraded,notes\nrun,2.0,,,,,0,0,0.0,running,,,[],\n"
    );
    drop(db);
    drop(telemetry);
    fs::remove_dir_all(dir).unwrap();
}
