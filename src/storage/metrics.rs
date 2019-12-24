// Copyright 2016 TiKV Project Authors. Licensed under Apache-2.0.

use prometheus::*;
use prometheus_static_metric::*;

use std::cell::RefCell;
use std::mem;
use std::time::Duration;

use crate::storage::kv::{FlowStatistics, FlowStatsReporter, Statistics};
use tikv_util::collections::HashMap;
use tikv_util::metrics::*;

struct StorageLocalMetrics {
    local_scan_details: HashMap<&'static str, Statistics>,
    local_read_flow_stats: HashMap<u64, FlowStatistics>,
}

thread_local! {
    static TLS_STORAGE_METRICS: RefCell<StorageLocalMetrics> = RefCell::new(
        StorageLocalMetrics {
            local_scan_details: HashMap::default(),
            local_read_flow_stats: HashMap::default(),
        }
    );
}

pub fn tls_flush<R: FlowStatsReporter>(reporter: &R) {
    TLS_STORAGE_METRICS.with(|m| {
        let mut m = m.borrow_mut();
        // Flush Prometheus metrics

        for (cmd, stat) in m.local_scan_details.drain() {
            for (cf, cf_details) in stat.details().iter() {
                for (tag, count) in cf_details.iter() {
                    KV_COMMAND_SCAN_DETAILS
                        .with_label_values(&[cmd, *cf, *tag])
                        .inc_by(*count as i64);
                }
            }
        }

        // Report PD metrics
        if m.local_read_flow_stats.is_empty() {
            // Stats to report to PD is empty, ignore.
            return;
        }

        let mut read_stats = HashMap::default();
        mem::swap(&mut read_stats, &mut m.local_read_flow_stats);

        reporter.report_read_stats(read_stats);
    });
}

pub fn tls_collect_command_count(cmd: CommandKind, priority: CommandPriority) {
    KV_COMMAND_COUNTER_VEC_STATIC.may_flush(|m| {
        m.get(cmd).inc();
    });

    SCHED_COMMANDS_PRI_COUNTER_VEC_STATIC.with(|m| {
        m.get(priority).inc();
    })
}

pub fn tls_collect_command_duration(cmd: CommandKind, duration: Duration) {
    SCHED_HISTOGRAM_VEC_STATIC.may_flush(|m| {
        m.get(cmd)
            .observe(tikv_util::time::duration_to_sec(duration))
    });
}

pub fn tls_collect_key_reads(cmd: CommandKind, count: usize) {
    KEYREAD_HISTOGRAM_VEC_STATIC.may_flush(|m| m.get(cmd).observe(count as f64))
}

pub fn tls_processing_read_observe_duration<F, R>(cmd: CommandKind, f: F) -> R
where
    F: FnOnce() -> R,
{
    SCHED_PROCESSING_VEC_STATIC.may_flush(|m| {
        let now = tikv_util::time::Instant::now_coarse();
        let ret = f();
        m.get(cmd).observe(now.elapsed_secs());
        ret
    })
}

pub fn tls_collect_scan_details(cmd: CommandKind, stats: &Statistics) {
    TLS_STORAGE_METRICS.with(|m| {
        m.borrow_mut()
            .local_scan_details
            .entry(cmd.get_str())
            .or_insert_with(Default::default)
            .add(stats);
    });
}

pub fn tls_collect_read_flow(region_id: u64, statistics: &Statistics) {
    TLS_STORAGE_METRICS.with(|m| {
        let map = &mut m.borrow_mut().local_read_flow_stats;
        let flow_stats = map.entry(region_id).or_insert_with(FlowStatistics::default);
        flow_stats.add(&statistics.write.flow_stats);
        flow_stats.add(&statistics.data.flow_stats);
    });
}

make_static_metric! {
    pub label_enum CommandKind {
        get,
        batch_get_command,
        batch_get,
        scan,
        raw_batch_get_command,
        prewrite,
        acquire_pessimistic_lock,
        commit,
        cleanup,
        rollback,
        pessimistic_rollback,
        txn_heart_beat,
        check_txn_status,
        scan_lock,
        resolve_lock,
        resolve_lock_lite,
        delete_range,
        pause,
        key_mvcc,
        start_ts_mvcc,
        raw_get,
        raw_batch_get,
        raw_scan,
        raw_batch_scan,
        raw_put,
        raw_batch_put,
        raw_delete,
        raw_delete_range,
        raw_batch_delete,
    }

    pub label_enum CommandStageKind {
        new,
        snapshot,
        async_snapshot_err,
        snapshot_ok,
        snapshot_err,
        read_finish,
        next_cmd,
        lock_wait,
        process,
        prepare_write_err,
        write,
        write_finish,
        async_write_err,
        error,
    }

    pub label_enum CommandPriority {
        low,
        normal,
        high,
    }

    pub struct SchedDurationVec: LocalHistogram {
        "type" => CommandKind,
    }
    pub struct SchedProcessingReadVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct KvCommandCounterVec: LocalIntCounter {
        "type" => CommandKind,
    }

    pub struct SchedStageCounterVec: IntCounter {
        "type" => CommandKind,
        "stage" => CommandStageKind,
    }

    pub struct SchedLatchDurationVec: Histogram {
        "type" => CommandKind,
    }

    pub struct KvCommandKeysReadVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct KvCommandKeysWrittenVec: Histogram {
        "type" => CommandKind,
    }

    pub struct SchedTooBusyVec: IntCounter {
        "type" => CommandKind,
    }

    pub struct SchedCommandPriCounterVec: LocalIntCounter {
        "priority" => CommandPriority,
    }
}

lazy_static! {
    pub static ref KV_COMMAND_COUNTER_VEC: IntCounterVec = register_int_counter_vec!(
        "tikv_storage_command_total",
        "Total number of commands received.",
        &["type"]
    )
    .unwrap();
    pub static ref SCHED_STAGE_COUNTER_VEC: SchedStageCounterVec = {
        register_static_int_counter_vec!(
            SchedStageCounterVec,
            "tikv_scheduler_stage_total",
            "Total number of commands on each stage.",
            &["type", "stage"]
        )
        .unwrap()
    };
    pub static ref SCHED_WRITING_BYTES_GAUGE: IntGauge = register_int_gauge!(
        "tikv_scheduler_writing_bytes",
        "Total number of writing kv."
    )
    .unwrap();
    pub static ref SCHED_CONTEX_GAUGE: IntGauge = register_int_gauge!(
        "tikv_scheduler_contex_total",
        "Total number of pending commands."
    )
    .unwrap();
    pub static ref SCHED_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_command_duration_seconds",
        "Bucketed histogram of command execution",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_LATCH_HISTOGRAM_VEC: SchedLatchDurationVec =
        register_static_histogram_vec!(
            SchedLatchDurationVec,
            "tikv_scheduler_latch_wait_duration_seconds",
            "Bucketed histogram of latch wait",
            &["type"],
            exponential_buckets(0.0005, 2.0, 20).unwrap()
        )
        .unwrap();
    pub static ref SCHED_PROCESSING_READ_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_processing_read_duration_seconds",
        "Bucketed histogram of processing read duration",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_PROCESSING_WRITE_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_processing_write_duration_seconds",
        "Bucketed histogram of processing write duration",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_TOO_BUSY_COUNTER_VEC: SchedTooBusyVec = register_static_int_counter_vec!(
        SchedTooBusyVec,
        "tikv_scheduler_too_busy_total",
        "Total count of scheduler too busy",
        &["type"]
    )
    .unwrap();
    pub static ref SCHED_COMMANDS_PRI_COUNTER_VEC: IntCounterVec = register_int_counter_vec!(
        "tikv_scheduler_commands_pri_total",
        "Total count of different priority commands",
        &["priority"]
    )
    .unwrap();
    pub static ref KV_COMMAND_KEYREAD_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_kv_command_key_read",
        "Bucketed histogram of keys read of a kv command",
        &["type"],
        exponential_buckets(1.0, 2.0, 21).unwrap()
    )
    .unwrap();
    pub static ref KV_COMMAND_SCAN_DETAILS: IntCounterVec = register_int_counter_vec!(
        "tikv_scheduler_kv_scan_details",
        "Bucketed counter of kv keys scan details for each cf",
        &["req", "cf", "tag"]
    )
    .unwrap();
    pub static ref KV_COMMAND_KEYWRITE_HISTOGRAM_VEC: KvCommandKeysWrittenVec =
        register_static_histogram_vec!(
            KvCommandKeysWrittenVec,
            "tikv_scheduler_kv_command_key_write",
            "Bucketed histogram of keys write of a kv command",
            &["type"],
            exponential_buckets(1.0, 2.0, 21).unwrap()
        )
        .unwrap();
    pub static ref REQUEST_EXCEED_BOUND: IntCounter = register_int_counter!(
        "tikv_request_exceed_bound",
        "Counter of request exceed bound"
    )
    .unwrap();
}

thread_local! {
    pub static SCHED_HISTOGRAM_VEC_STATIC: TLSMetricGroup<SchedDurationVec> =
         TLSMetricGroup::new(SchedDurationVec::from(&SCHED_HISTOGRAM_VEC));
    pub static SCHED_PROCESSING_VEC_STATIC: TLSMetricGroup<SchedProcessingReadVec> =
         TLSMetricGroup::new(SchedProcessingReadVec::from(&SCHED_PROCESSING_READ_HISTOGRAM_VEC));
    pub static KEYREAD_HISTOGRAM_VEC_STATIC: TLSMetricGroup<KvCommandKeysReadVec> =
         TLSMetricGroup::new(KvCommandKeysReadVec::from(&KV_COMMAND_KEYREAD_HISTOGRAM_VEC));
    pub static KV_COMMAND_COUNTER_VEC_STATIC: TLSMetricGroup<KvCommandCounterVec> =
        TLSMetricGroup::new(KvCommandCounterVec::from(&KV_COMMAND_COUNTER_VEC));
    pub static SCHED_COMMANDS_PRI_COUNTER_VEC_STATIC: TLSMetricGroup<SchedCommandPriCounterVec> =
        TLSMetricGroup::new(SchedCommandPriCounterVec::from(&SCHED_COMMANDS_PRI_COUNTER_VEC));
}
