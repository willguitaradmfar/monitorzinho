use crate::history::{self, CAPACITY, History};
use crate::monitor::{self, Monitor, SystemState, TableMonitor, TableRow};

const SAVE_EVERY_N_TICKS: u32 = 5;

pub struct App {
    pub monitors: Vec<Box<dyn Monitor>>,
    pub histories: Vec<History>,
    pub extras: Vec<Option<String>>,
    pub capacities: Vec<Option<f64>>,
    pub table_monitors: Vec<Box<dyn TableMonitor>>,
    pub table_rows: Vec<Vec<TableRow>>,
    state: SystemState,
    ticks_since_save: u32,
}

impl App {
    pub fn new() -> Self {
        let monitors = monitor::all_monitors();
        let saved = history::load_all();
        let histories = monitors
            .iter()
            .map(|m| match saved.get(m.id()) {
                Some(values) => History::from_saved(values.clone(), CAPACITY),
                None => History::new(CAPACITY),
            })
            .collect();
        let extras = monitors.iter().map(|_| None).collect();
        let capacities = monitors.iter().map(|_| None).collect();

        let table_monitors = monitor::all_table_monitors();
        let table_rows = table_monitors.iter().map(|_| Vec::new()).collect();

        Self {
            monitors,
            histories,
            extras,
            capacities,
            table_monitors,
            table_rows,
            state: SystemState::new(),
            ticks_since_save: 0,
        }
    }

    pub fn tick(&mut self) {
        self.state.refresh();
        for (((monitor, history), extra), capacity) in self
            .monitors
            .iter_mut()
            .zip(self.histories.iter_mut())
            .zip(self.extras.iter_mut())
            .zip(self.capacities.iter_mut())
        {
            let value = monitor.sample(&self.state);
            history.push(value);
            *extra = monitor.extra(&self.state);
            *capacity = monitor.capacity(&self.state);
        }
        for (monitor, rows) in self
            .table_monitors
            .iter_mut()
            .zip(self.table_rows.iter_mut())
        {
            *rows = monitor.sample(&self.state);
        }

        self.ticks_since_save += 1;
        if self.ticks_since_save >= SAVE_EVERY_N_TICKS {
            self.persist();
            self.ticks_since_save = 0;
        }
    }

    pub fn persist(&self) {
        let mut map = history::HistoryMap::new();
        for (monitor, history) in self.monitors.iter().zip(self.histories.iter()) {
            map.insert(monitor.id().to_string(), history.values());
        }
        history::save_all(&map);
    }
}
