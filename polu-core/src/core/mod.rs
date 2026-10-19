pub(crate) mod traits;
pub(crate) mod types;

use types::{CostMap, SinkList, SourceList};

use traits::{Load, Solver, Store};

pub trait PoluCore {
    ///
    /// The main trait for what a Polu implementation should be equipped to do. It is intended to work as an ETL process mainly.
    /// Data loading and results return a reference that lives as long as the implementor type
    /// 

    // domain types
    // type SourceList;
    // type SinkList;
    // type CostMap;


    // lifted types
    type DataStatement;
    type Solution;


    // components
    type SourceLoader: Load<SourceList>;
    type SinkLoader: Load<SinkList>;
    type CostMapLoader: Load<CostMap>;

    type Solver: Solver<
        DataStatement = Self::DataStatement,
        Solution = Self::Solution,
    >;

    type Store: Store<Self::Solution>;

    // errors
    type LoadError: From<<Self::SourceLoader as Load<SourceList>>::Error>
        + From<<Self::SinkLoader as Load<SinkList>>::Error>
        + From<<Self::CostMapLoader as Load<CostMap>>::Error>
        + std::error::Error;



    fn load_data(&mut self) -> Result<&Self::DataStatement, Self::LoadError>;

    fn run(
        &mut self,
        data: &Self::DataStatement,
    ) -> Result<&Self::Solution, <Self::Solver as Solver>::Error>;

    fn store_data(&self) -> Result<(), <Self::Store as Store<Self::Solution>>::Error>;
}

struct Polu<T: PoluCore> {
    source_loader: T::SourceLoader,
    sink_loader: T::SinkLoader,
    costmap_loader: T::CostMapLoader,
    solver: T::Solver,
    storer: T::Store,
    sources: Option<SourceList>,
    sinks: Option<SinkList>,
    costmap: Option<CostMap>,
    solver_data: Option<T::DataStatement>,
    solver_result: Option<T::Solution>,
}

impl<T> Polu<T>
where
    T: PoluCore,
{
    pub fn new(
        source_loader: T::SourceLoader,
        sink_loader: T::SinkLoader,
        costmap_loader: T::CostMapLoader,
        solver: T::Solver,
        storer: T::Store,
    ) -> Self {
        Self {
            source_loader,
            sink_loader,
            costmap_loader,
            solver,
            storer,
            sources: None,
            sinks: None,
            costmap: None,
            solver_data: None,
            solver_result: None,
        }
    }

    pub fn load_data(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.source_loader
            .load()
            .and_then(|x| Ok(self.sources.insert(x)))?;
        self.sink_loader
            .load()
            .and_then(|x| Ok(self.sinks.insert(x)))?;
        self.costmap_loader
            .load()
            .and_then(|x| Ok(self.costmap.insert(x)))?;

        self.solver_data.insert(self.solver.prepare(
            self.sources.as_ref().unwrap(),
            self.sinks.as_ref().unwrap(),
            self.costmap.as_ref().unwrap(),
        ));

        Ok(()) 
    }

    pub fn run(
        &mut self,
    ) -> Result<&T::Solution, <T::Solver as Solver>::Error> {
        let solution: <<T as PoluCore>::Solver as Solver>::Solution = self
            .solver_data
            .as_ref()
            .ok_or_else(Default::default)
            .and_then(|x| self.solver.solve(x))?;

        Ok(self.solver_result.insert(solution))
    }

    pub fn store_data(
        &self,
    ) -> Result<(), <T::Store as Store<<T::Solver as Solver>::Solution>>::Error> {
        self.solver_result
            .as_ref()
            .ok_or_else(Default::default)
            .and_then(|x| self.storer.store(x))
    }
}
