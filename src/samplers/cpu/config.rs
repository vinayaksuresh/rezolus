// Copyright 2019-2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use rustcommon_atomics::*;
use serde_derive::Deserialize;
use strum::IntoEnumIterator;

use crate::config::SamplerConfig;

use super::stat::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuConfig {
    #[serde(default)]
    enabled: AtomicBool,
    #[serde(default)]
    interval: Option<AtomicUsize>,
    #[serde(default = "crate::common::default_percentiles")]
    percentiles: Vec<f64>,
    #[serde(default)]
    perf_events: AtomicBool,
    #[serde(default = "default_statistics")]
    statistics: Vec<CpuStatistic>,
}

impl Default for CpuConfig {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            interval: Default::default(),
            percentiles: crate::common::default_percentiles(),
            perf_events: Default::default(),
            statistics: default_statistics(),
        }
    }
}

fn default_statistics() -> Vec<CpuStatistic> {
    CpuStatistic::iter().collect()
}

impl SamplerConfig for CpuConfig {
    type Statistic = CpuStatistic;
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn interval(&self) -> Option<usize> {
        self.interval.as_ref().map(|v| v.load(Ordering::Relaxed))
    }

    fn percentiles(&self) -> &[f64] {
        &self.percentiles
    }

    fn perf_events(&self) -> bool {
        self.perf_events.load(Ordering::Relaxed)
    }

    fn statistics(&self) -> Vec<<Self as SamplerConfig>::Statistic> {
        let mut enabled = Vec::new();
        for statistic in self.statistics.iter() {
            if statistic.table().is_some() {
                if self.perf_events() {
                    enabled.push(statistic.clone());
                }
            } else {
                enabled.push(statistic.clone());
            }
        }
        enabled
    }
}
