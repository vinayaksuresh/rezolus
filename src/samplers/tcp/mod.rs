// Copyright 2019-2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use std::sync::{Arc, Mutex};
use std::time::*;
use tokio::fs::File;

use async_trait::async_trait;

use crate::common::bpf::*;
use crate::config::SamplerConfig;
use crate::samplers::{Common, Sampler};

mod config;
mod stat;

pub use config::*;
pub use stat::*;

#[allow(dead_code)]
pub struct Tcp {
    bpf: Option<Arc<Mutex<BPF>>>,
    bpf_last: Arc<Mutex<Instant>>,
    common: Common,
    proc_net_snmp: Option<File>,
    proc_net_netstat: Option<File>,
    statistics: Vec<TcpStatistic>,
}

#[async_trait]
impl Sampler for Tcp {
    type Statistic = TcpStatistic;
    fn new(common: Common) -> Result<Self, anyhow::Error> {
        let fault_tolerant = common.config.general().fault_tolerant();
        let statistics = common.config().samplers().tcp().statistics();

        #[allow(unused_mut)]
        let mut sampler = Self {
            bpf: None,
            bpf_last: Arc::new(Mutex::new(Instant::now())),
            common,
            proc_net_snmp: None,
            proc_net_netstat: None,
            statistics,
        };

        if let Err(e) = sampler.initialize_bpf() {
            if !fault_tolerant {
                return Err(e);
            }
        }

        if sampler.sampler_config().enabled() {
            sampler.register();
        }

        Ok(sampler)
    }

    fn spawn(common: Common) {
        if common.config().samplers().tcp().enabled() {
            if let Ok(mut sampler) = Self::new(common.clone()) {
                common.handle.spawn(async move {
                    loop {
                        let _ = sampler.sample().await;
                    }
                });
            } else if !common.config.fault_tolerant() {
                fatal!("failed to initialize tcp sampler");
            } else {
                error!("failed to initialize tcp sampler");
            }
        }
    }

    fn common(&self) -> &Common {
        &self.common
    }

    fn common_mut(&mut self) -> &mut Common {
        &mut self.common
    }

    fn sampler_config(&self) -> &dyn SamplerConfig<Statistic = Self::Statistic> {
        self.common.config().samplers().tcp()
    }

    async fn sample(&mut self) -> Result<(), std::io::Error> {
        if let Some(ref mut delay) = self.delay() {
            delay.tick().await;
        }

        if !self.sampler_config().enabled() {
            return Ok(());
        }

        debug!("sampling");

        let r = self.sample_snmp().await;
        self.map_result(r)?;

        let r = self.sample_netstat().await;
        self.map_result(r)?;

        // sample bpf
        #[cfg(feature = "bpf")]
        self.map_result(self.sample_bpf())?;

        Ok(())
    }
}

impl Tcp {
    // checks that bpf is enabled in config and one or more bpf stats enabled
    #[cfg(feature = "bpf")]
    fn bpf_enabled(&self) -> bool {
        if self.sampler_config().bpf() {
            for statistic in self.sampler_config().statistics() {
                if statistic.bpf_table().is_some() {
                    return true;
                }
            }
        }
        false
    }

    fn initialize_bpf(&mut self) -> Result<(), anyhow::Error> {
        #[cfg(feature = "bpf")]
        {
            if self.enabled() && self.bpf_enabled() {
                debug!("initializing bpf");
                // load the code and compile
                let code = include_str!("bpf.c");
                let mut bpf = bcc::BPF::new(code)?;

                // load + attach kprobes!
                bcc::Kprobe::new()
                    .handler("trace_connect")
                    .function("tcp_v4_connect")
                    .attach(&mut bpf)?;
                bcc::Kprobe::new()
                    .handler("trace_connect")
                    .function("tcp_v6_connect")
                    .attach(&mut bpf)?;
                bcc::Kprobe::new()
                    .handler("trace_tcp_rcv_state_process")
                    .function("tcp_rcv_state_process")
                    .attach(&mut bpf)?;

                self.bpf = Some(Arc::new(Mutex::new(BPF { inner: bpf })))
            }
        }

        Ok(())
    }

    async fn sample_snmp(&mut self) -> Result<(), std::io::Error> {
        if self.proc_net_snmp.is_none() {
            let file = File::open("/proc/net/snmp").await?;
            self.proc_net_snmp = Some(file);
        }
        if let Some(file) = &mut self.proc_net_snmp {
            let parsed = crate::common::nested_map_from_file(file).await?;
            let time = Instant::now();
            for statistic in &self.statistics {
                if let Some((pkey, lkey)) = statistic.keys() {
                    if let Some(inner) = parsed.get(pkey) {
                        if let Some(value) = inner.get(lkey) {
                            let _ = self.metrics().record_counter(statistic, time, *value);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn sample_netstat(&mut self) -> Result<(), std::io::Error> {
        if self.proc_net_netstat.is_none() {
            let file = File::open("/proc/net/netstat").await?;
            self.proc_net_netstat = Some(file);
        }
        if let Some(file) = &mut self.proc_net_netstat {
            let parsed = crate::common::nested_map_from_file(file).await?;
            let time = Instant::now();
            for statistic in &self.statistics {
                if let Some((pkey, lkey)) = statistic.keys() {
                    if let Some(inner) = parsed.get(pkey) {
                        if let Some(value) = inner.get(lkey) {
                            let _ = self.metrics().record_counter(statistic, time, *value);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "bpf")]
    fn sample_bpf(&self) -> Result<(), std::io::Error> {
        if self.bpf_last.lock().unwrap().elapsed()
            >= Duration::new(self.general_config().window() as u64, 0)
        {
            if let Some(ref bpf) = self.bpf {
                let bpf = bpf.lock().unwrap();
                let time = Instant::now();
                for statistic in self.statistics.iter().filter(|s| s.bpf_table().is_some()) {
                    if let Ok(mut table) = (*bpf).inner.table(statistic.bpf_table().unwrap()) {
                        for (&value, &count) in &map_from_table(&mut table) {
                            if count > 0 {
                                let _ = self.metrics().record_bucket(
                                    statistic,
                                    time,
                                    value * 1000,
                                    count,
                                );
                            }
                        }
                    }
                }
            }
            *self.bpf_last.lock().unwrap() = Instant::now();
        }
        Ok(())
    }
}
