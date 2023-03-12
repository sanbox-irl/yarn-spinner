#![doc = include_str!("../README.md")]
// #![cfg_attr(not(feature = "std"), doc = "no-std stand in")]
#![deny(unsafe_code)]
#![deny(rust_2018_idioms)]
// #![deny(missing_docs)]
#![warn(clippy::dbg_macro)]
#![warn(clippy::print_stdout)]
// #![deny(clippy::missing_errors_doc)]
// #![warn(clippy::missing_panics_doc)]
#![warn(clippy::todo)]
#![warn(clippy::undocumented_unsafe_blocks)]
#![deny(rustdoc::broken_intra_doc_links)]

mod byte_code;
mod proto;
mod storage;
mod localization_handler;

// utility
mod type_names {
    pub const F32: &str = "f32";
    pub const STR: &str = "str";
    pub const BOOL: &str = "bool";
}

use byte_code::Instruction;
pub use byte_code::{YarnProgram, YarnProgramError};

pub use storage::YarnStorage;

/// A Virtual Machine which executes a [YarnProgram].
#[derive(Debug)]
pub struct YarnRunner {
    program: YarnProgram,
    runner_state: RunnerState,

    state: Option<State>,
}

impl YarnRunner {
    /// Creates a new [YarnRunner] with the given Program.
    pub fn new(program: YarnProgram) -> Self {
        Self {
            program,
            runner_state: RunnerState::Stopped,
            state: None,
        }
    }

    /// Get a reference to the inner yarn program
    pub fn program(&self) -> &YarnProgram {
        &self.program
    }

    pub fn set_node(&mut self, node_name: String) -> Result<(), NodeDoesNotExist> {
        if !self.program.nodes.contains_key(&node_name) {
            return Err(NodeDoesNotExist(node_name));
        }

        // perf: we could reuse the stack + options in some manner
        self.state = Some(State {
            node_name,
            instruction: 0,
            stack: vec![],
            options: vec![],
        });

        self.runner_state = RunnerState::WaitingForContinue;

        Ok(())
    }

    pub fn execute(
        &mut self,
        storage: &mut YarnStorage,
    ) -> Result<Option<ExecutionOutput>, ExecutionError> {
        let Some(mut state) = self.state.as_mut() else {
            return Err(ExecutionError::NotReadyToExecute(NotReadyToExecute::NoNodeSelected));
         };

        if matches!(
            self.runner_state,
            RunnerState::WaitingForOptionSelection { .. }
        ) {
            return Err(ExecutionError::NotReadyToExecute(
                NotReadyToExecute::WaitingForOptionSelection,
            ));
        }

        let mut node = self
            .program
            .nodes
            .get(&state.node_name)
            .expect("we already checked that this was valid");

        loop {
            // save old instruction counter and increment it
            let instruction_count = state.instruction;
            state.instruction += 1;

            let Some(instruction) = node.instructions.get(instruction_count) else { break };

            match instruction {
                Instruction::JumpTo(node_name) => {
                    state.instruction = *node
                        .jump_table
                        .get(node_name)
                        .expect("is the compiler wrong here?");
                }
                Instruction::JumpIfFalse(node_name) => {
                    let stack_top = state.stack.last().expect("is this possible?");
                    if stack_top.try_to_bool()? {
                        state.instruction = *node
                            .jump_table
                            .get(node_name)
                            .expect("is the compiler wrong here?");
                    }
                }
                Instruction::Jump => {
                    state.instruction = *node
                        .jump_table
                        .get(&state.stack.last().expect("must handle").to_string())
                        .expect("is the compiler wrong here?");
                }
                Instruction::RunLine(line) => {
                    let mut substitutions = vec![];
                    for _ in 0..line.substitution_count {
                        substitutions.push(state.stack.pop().expect("handle errr").to_string());
                    }

                    return Ok(Some(ExecutionOutput::Line(YarnLine {
                        string_key: line.string_key.clone(),
                        substitutions,
                    })));
                }
                Instruction::Command(command_datas) => {
                    let mut substitutions = vec![];
                    for _ in 0..command_datas.substitution_count {
                        substitutions.push(state.stack.pop().expect("handle errr").to_string());
                    }

                    let command = apply_arguments_in_substition(
                        &command_datas.command_string,
                        &substitutions,
                    );

                    return Ok(Some(ExecutionOutput::Command(command)));
                }
                Instruction::AddOption(option_data) => {
                    let condition_passed = if option_data.has_condition {
                        Some(state.stack.pop().expect("must handle").try_to_bool()?)
                    } else {
                        None
                    };

                    let mut substitutions = vec![];

                    for _ in 0..option_data.line.substitution_count {
                        substitutions.push(state.stack.pop().expect("handle errr").to_string());
                    }

                    state.options.push(YarnOption {
                        line: YarnLine {
                            string_key: option_data.line.string_key.clone(),
                            substitutions,
                        },
                        destination: option_data.destination.clone(),
                        condition_passed,
                    });
                }
                Instruction::ShowOptions => {
                    // give em da options!
                    return Ok(Some(ExecutionOutput::Options(std::mem::take(
                        &mut state.options,
                    ))));
                }
                Instruction::PushString(str) => {
                    state.stack.push(YarnValue::Str(str.clone()));
                }
                Instruction::PushFloat(f) => {
                    state.stack.push(YarnValue::F32(*f));
                }
                Instruction::PushBool(b) => {
                    state.stack.push(YarnValue::Bool(*b));
                }
                Instruction::Pop => {
                    state.stack.pop();
                }
                Instruction::CallFunc(func_call) => {
                    panic!("we don't support functions yet")
                }
                Instruction::PushVar(v) => {
                    let value = match storage.get(v) {
                        Some(v) => v,
                        None => {
                            // okay let's check the default type listings
                            match self.program.initial_values.get(v) {
                                Some(v) => v,
                                None => return Err(ExecutionError::UnknownVariable(v.to_string())),
                            }
                        }
                    };

                    state.stack.push(value.clone());
                }
                Instruction::StoreVar(value_name) => {
                    let value = state.stack.last().expect("must handle").clone();

                    storage.insert(value_name.clone(), value);
                }
                Instruction::Stop => {
                    self.runner_state = RunnerState::Stopped;
                    self.state = None;

                    break;
                }
                Instruction::RunNode => {
                    let node_name = state.stack.pop().expect("must handle").to_string();

                    // node completion handle?
                    self.set_node(node_name)?;

                    state = self.state.as_mut().unwrap();
                    node = self
                        .program
                        .nodes
                        .get(&state.node_name)
                        .expect("we already checked that this was valid");
                }
            }
        }

        // This also means we reached the end of our program's execution!
        Ok(None)
    }
}

#[derive(Debug)]
struct State {
    node_name: String,
    instruction: usize,
    stack: Vec<YarnValue>,
    options: Vec<YarnOption>,
}

#[derive(Debug)]
pub enum ExecutionOutput {
    Line(YarnLine),
    Options(Vec<YarnOption>),
    Command(String),
}

/// This represents the current state of the [YarnRunner].
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum RunnerState {
    /// The [YarnRunner] is not running a node.
    Stopped,

    /// The [YarnRunner] is waiting for an option to be selected with `select_option`.
    WaitingForOptionSelection {
        selection: Option<usize>,
        count: usize,
    },

    /// The [YarnRunner] is waiting for `continue` to be called.
    WaitingForContinue,
}

/// Yarn supports three kinds of values: f32s, bools, and Strings.
#[derive(Debug, PartialEq, PartialOrd, Clone)]
pub enum YarnValue {
    /// A boolean
    Bool(bool),
    /// An owned string
    Str(String),
    /// An f32
    F32(f32),
}

impl std::fmt::Display for YarnValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YarnValue::Bool(v) => v.fmt(f),
            YarnValue::Str(v) => v.fmt(f),
            YarnValue::F32(v) => v.fmt(f),
        }
    }
}

impl YarnValue {
    /// Converts a given value to a bool.
    pub fn try_to_bool(&self) -> Result<bool, ConversionError> {
        let v = match self {
            YarnValue::Bool(v) => *v,
            YarnValue::Str(s) => {
                // these rules need to be taken from the C# source
                match s.as_str() {
                    // the C# implementation actually supports arbitrary capitalization
                    // but we're not supporting out for the sake of my typing
                    "true" | "True" => true,
                    "false" | "FALSE" => false,
                    _ => {
                        return Err(ConversionError {
                            target_type: type_names::BOOL,
                            found_type: type_names::STR,
                        })
                    }
                }
            }
            YarnValue::F32(f) => *f != 0.0,
        };

        Ok(v)
    }

    /// Converts a given value to a string.
    pub fn try_to_f32(&self) -> Result<f32, ConversionError> {
        let v = match self {
            YarnValue::Bool(v) => {
                if *v {
                    1.0
                } else {
                    0.0
                }
            }
            YarnValue::Str(s) => s.parse::<f32>().map_err(|_| ConversionError {
                target_type: type_names::F32,
                found_type: type_names::STR,
            })?,
            YarnValue::F32(f) => *f,
        };

        Ok(v)
    }
}

// this is identical to `Line` in `bytecode` but I've made them separate types so
// we have more flexibility with semver.
#[derive(Debug)]
pub struct YarnLine {
    pub(crate) string_key: String,
    pub(crate) substitutions: Vec<String>,
}

#[derive(Debug)]
pub struct YarnOption {
    pub(crate) line: YarnLine,
    pub(crate) destination: String,

    /// if `None`, then this line never had a condition. If Some, the status is if it passed.
    pub(crate) condition_passed: Option<bool>,
}

/// Applies substitutions to a given line as needed.
pub fn apply_arguments_in_substition(f_string: &str, subs: &[String]) -> String {
    let mut output = f_string.to_owned();

    for (i, sub) in subs.iter().enumerate() {
        output = output.replace(&format!("{{{i}}}"), sub);
    }

    output
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error(transparent)]
    NotReadyToExecute(NotReadyToExecute),

    #[error(transparent)]
    NodeDoesNotExist(#[from] NodeDoesNotExist),

    #[error(transparent)]
    ConversionError(#[from] ConversionError),

    #[error("unknown variable binding `{0}`")]
    UnknownVariable(String),
}

#[derive(Debug, thiserror::Error)]
#[error("A node of name {0} is not present in the currently loaded program.")]
pub struct NodeDoesNotExist(String);

#[derive(Debug, thiserror::Error)]
pub enum NotReadyToExecute {
    #[error("waiting for option to be selected. call `select_option`")]
    WaitingForOptionSelection,
    #[error("no node selected. call `set_node`")]
    NoNodeSelected,
}

#[derive(Debug, thiserror::Error)]
#[error("value of type `{target_type}` could not be converted to `{found_type}`")]
pub struct ConversionError {
    target_type: &'static str,
    found_type: &'static str,
}
