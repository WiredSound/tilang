use crate::checking;
use super::Generator;

pub fn input(instructions: Vec<checking::Instruction>) -> String {
    GenerateElf64::new().execute(instructions)
}

trait AssemblyDisplay {
    fn intel_syntax(self) -> String;
    fn at_and_t_syntax(self) -> String where Self: Sized { unimplemented!() }
}

#[derive(Clone)]
enum Instruction {
    Section(String),
    Global(String),
    Label(String),
    Syscall,
    Mov { dest: Oprand, src: Oprand },
    Add { dest: Oprand, src: Oprand },
    Push(Oprand),
    Pop(Oprand),
    FpuPush(Oprand),
    FpuAdd,
    FpuPop(Oprand),
    Declare(Val),
    Ret(usize),
    Call(String),
    Jmp(String)
}

impl AssemblyDisplay for Instruction {
    fn intel_syntax(self) -> String {
        match self {
            Instruction::Section(x) => format!("section .{}\n", x),
            Instruction::Global(x) => format!("global {}\n", x),
            Instruction::Label(x) => format!("{}:\n", x),
            Instruction::Syscall => format!("syscall\n"),
            Instruction::Mov { dest, src } => format!("mov {}, {}\n", dest.intel_syntax(), src.intel_syntax()),
            Instruction::Add { dest, src } => format!("add {}, {}\n", dest.intel_syntax(), src.intel_syntax()),
            Instruction::Push(x) => format!("push qword {}\n", x.intel_syntax()),
            Instruction::Pop(x) => format!("pop qword {}\n", x.intel_syntax()),
            Instruction::FpuPush(x) => format!("fld qword {}\n", x.intel_syntax()),
            Instruction::FpuAdd => format!("fadd\n"),
            Instruction::FpuPop(x) => format!("fst qword {}\n", x.intel_syntax()),
            Instruction::Declare(x) => format!("dq {}\n", x.intel_syntax()),
            Instruction::Ret(x) => format!("ret {}\n", x),
            Instruction::Call(x) => format!("call {}\n", x),
            Instruction::Jmp(x) => format!("jmp {}\n", x)
        }
    }
}

#[derive(Clone)]
enum Oprand {
    Label(String),
    Value(Val),
    Register(Reg),
    Address(Box<Oprand>),
    AddressDisplaced(Box<Oprand>, usize),
}

impl AssemblyDisplay for Oprand {
    fn intel_syntax(self) -> String {
        match self {
            Oprand::Label(x) => x,
            Oprand::Value(x) => x.intel_syntax(),
            Oprand::Register(x) => format!("{:?}", x),
            Oprand::Address(x) => format!("[{}]", x.intel_syntax()),
            Oprand::AddressDisplaced(x, displacement) => format!("[{} + {}]", x.intel_syntax(), displacement)
        }
    }
}

#[derive(Clone)]
enum Val { Int(isize), Float(f64) }

impl AssemblyDisplay for Val {
    fn intel_syntax(self) -> String {
        match self {
            Val::Int(x) => x.to_string(),
            Val::Float(x) => format!("{:.16}", x)
        }
    }
}

#[derive(Clone, Debug)]
enum Reg { Rax, Rbx, Rsp, Rbp, Rdi }

struct GenerateElf64 {
    text_section: Vec<Instruction>,
    data_section: Vec<Instruction>,
    rodata_section: Vec<Instruction>,
    num_label_counter: usize
}

impl GenerateElf64 {
    fn new() -> Self {
        GenerateElf64 {
            text_section: vec![
                Instruction::Section("text".to_string()),
                Instruction::Global("_start".to_string()),
                Instruction::Label("_start".to_string())
            ],
            data_section: vec![Instruction::Section("data".to_string())],
            rodata_section: vec![Instruction::Section("rodata".to_string())],
            num_label_counter: 0
        }
    }
}

const RETURN_INSTRUCTIONS: &'static [Instruction] = &[
    Instruction::Pop(Oprand::Register(Reg::Rbp)), // Restore the base pointer of the previous frame.
    Instruction::Ret(16) // Shift stack pointer by 2 (remove old base pointer, return address) when returning.
];

impl Generator for GenerateElf64 {
    const TARGET_NAME: &'static str = "Linux elf64";

    fn handle_instruction(&mut self, instruction: checking::Instruction) {
        match instruction {
            checking::Instruction::Allocate(id) => {
                self.data_section.extend(vec![
                    Instruction::Label(var_label(id)),
                    Instruction::Declare(Val::Float(0.0))
                ]);
            }

            checking::Instruction::Push(val) => {
                let label = match val {
                    checking::Value::Num(num_val) => {
                        let label = literal_label(self.num_label_counter);
                        self.num_label_counter += 1;

                        self.rodata_section.extend(vec![
                            Instruction::Label(label.clone()),
                            Instruction::Declare(Val::Float(num_val))
                        ]);

                        label
                    }

                    checking::Value::Variable(var_id) => var_label(var_id),

                    _ => unimplemented!()
                };

                self.text_section.push(Instruction::Push(
                    Oprand::Address(Box::new(Oprand::Label(label)))
                ));
            }

            checking::Instruction::Store(id) => {
                let label = var_label(id);

                // Store value on top of stack in .data section:
                self.text_section.push(Instruction::Pop(
                    Oprand::Address(Box::new(Oprand::Label(label)))
                ));
            }

            checking::Instruction::Parameter { store_in, param_number } => {
                // Store function argument in parameter variable:
                self.text_section.extend(vec![
                    Instruction::Mov {
                        dest: Oprand::Register(Reg::Rbx),
                        src: Oprand::AddressDisplaced(Box::new(Oprand::Register(Reg::Rsp)), 16 + (param_number * 8))
                    },
                    Instruction::Mov {
                        dest: Oprand::Address(Box::new(Oprand::Label(var_label(store_in)))),
                        src: Oprand::Register(Reg::Rbx)
                    }
                ]);
            }

            checking::Instruction::Label(id) => { self.text_section.push(Instruction::Label(label(id))); }

            checking::Instruction::Function(id) => { 
                self.text_section.extend(vec![
                    Instruction::Label(func_label(id)),
                    // Preserve the base pointer of the previous frame:
                    Instruction::Push(Oprand::Register(Reg::Rbp)),
                    // Create a new frame beginning at the current stack top:
                    Instruction::Mov {
                        dest: Oprand::Register(Reg::Rbp),
                        src: Oprand::Register(Reg::Rsp)
                    }
                ]);
            }

            checking::Instruction::CallExpectingVoid(id) => { self.text_section.push(Instruction::Call(func_label(id))); }

            checking::Instruction::CallExpectingValue(id) => {
                self.text_section.extend(vec![
                    Instruction::Call(func_label(id)),
                    // Place the function return value on the stack:
                    Instruction::Push(Oprand::Register(Reg::Rax))
                ]);
            }

            checking::Instruction::ReturnVoid => { self.text_section.extend_from_slice(RETURN_INSTRUCTIONS); }

            checking::Instruction::ReturnValue => {
                // Place function return value in register:
                self.text_section.push(Instruction::Pop(Oprand::Register(Reg::Rax)));
                self.text_section.extend_from_slice(RETURN_INSTRUCTIONS);
            }

            checking::Instruction::Jump(id) => { self.text_section.push(Instruction::Jmp(label(id))); }

            checking::Instruction::Add => {
                self.text_section.extend(vec![
                    // Load top of stack onto FPU stack:
                    Instruction::FpuPush(Oprand::Address(Box::new(Oprand::Register(Reg::Rsp)))),
                    // Load second-to-top of stack onto FPU stack:
                    Instruction::FpuPush(Oprand::AddressDisplaced(Box::new(Oprand::Register(Reg::Rsp)), 8)),
                    Instruction::FpuAdd,
                    // Move stack pointer:
                    Instruction::Add { dest: Oprand::Register(Reg::Rsp), src: Oprand::Value(Val::Int(8)) },
                    // Store result on stack:
                    Instruction::FpuPop(Oprand::Address(Box::new(Oprand::Register(Reg::Rsp))))
                ]);
            }

            _ => {}
        }
    }

    fn construct_output(mut self) -> String {
        self.text_section.extend(vec![
            Instruction::Mov { dest: Oprand::Register(Reg::Rax), src: Oprand::Value(Val::Int(60)) },
            Instruction::Mov { dest: Oprand::Register(Reg::Rdi), src: Oprand::Value(Val::Int(0)) },
            Instruction::Syscall
        ]);
        self.text_section.extend(self.data_section.into_iter());
        self.text_section.extend(self.rodata_section.into_iter());

        self.text_section.into_iter().map(|x| x.intel_syntax()).collect::<Vec<String>>().join("")
    }
}

fn label(id: usize) -> String { format!("label{}", id) }

fn func_label(id: usize) -> String { format!("func{}", id) }

fn var_label(id: usize) -> String { format!("var{}", id) }

fn literal_label(counter: usize) -> String { format!("literal{}", counter) }

// TODO: Tests...