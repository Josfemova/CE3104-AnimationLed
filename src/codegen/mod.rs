//! Generación de código.
//!
//! Esta fase consiste en la traducción de representación
//! intermedia (véase [`crate::ir`]) a un lenguaje ensamblador
//! objetivo en particular. Este módulo :

use crate::{
    arch::{Arch, Emitter},
    ir::{Function, FunctionBody, Global, Instruction, Label, Local, Program},
};

use std::{
    cell::RefCell,
    fmt,
    io::{self, Write},
    ops::Deref,
};

/// Emite código ensamblador para un programa IR.
///
/// Esta función es el punto de entrada del mecanismo de generación
/// de código. Cada función es escrita al flujo de salida según
/// corresponda para la arquitectura objetivo. La salida está destinada
/// a ser utilizada directamente por el GNU assembler y no se esperan
/// otras interpretaciones o manipulaciones antes de ello.
pub fn emit(program: &Program, arch: Arch, output: &mut dyn Write) -> io::Result<()> {
    let value_size = dispatch_arch!(Emitter: arch => Emitter::VALUE_SIZE);

    // Variables globales van en .bss
    for global in &program.globals {
        let Global(name) = global.deref();
        writeln!(output, ".lcomm {}, {}", name, value_size)?;
    }

    // user_main() es un caso especial en lo que respecta a enlazado
    writeln!(output, ".text\n.global user_main")?;

    // Se emite propiamente cada función no externa
    for function in &program.code {
        if let FunctionBody::Generated(instructions) = &function.body {
            dispatch_arch!(Emitter: arch => {
                emit_body::<Emitter>(output, function, &instructions)?;
            });
        }
    }

    Ok(())
}

/// Contexto de emisión.
///
/// Esta estructura contiene información que las implementaciones
/// de emisión requieren con frecuencia, como lo son el flujo de salida
/// y la función IR que está siendo generada.
pub struct Context<'a, E: Emitter<'a>> {
    function: &'a Function,
    output: RefCell<&'a mut dyn Write>,
    locals: u32,
    frame_info: E::FrameInfo,
}

impl<'a, E: Emitter<'a>> Context<'a, E> {
    /// Función en forma IR que está siendo generada.
    pub fn function(&self) -> &Function {
        self.function
    }

    /// Escribe al flujo de salida.
    pub fn write_fmt(&self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.output.borrow_mut().write_fmt(fmt)
    }

    /// Cantidad máxima de locales que la función accede,
    /// recibe o utiliza en su forma IR.
    ///
    /// Este número se denomina "agnóstico" ya que algunas
    /// implementaciones pueden optar por insertar locales
    /// adicionales por razones que dependen de la arquitectura.
    pub fn agnostic_locals(&self) -> u32 {
        self.locals
    }

    /// Obtiene la información de marco de llamada actual.
    /// Sus contenidos y significado dependen de la arquitectura.
    pub fn frame_info(&self) -> &E::FrameInfo {
        &self.frame_info
    }

    /// Sustituye la información de marco actual para este contexto.
    pub fn with_frame_info(self, frame_info: E::FrameInfo) -> Self {
        Context { frame_info, ..self }
    }
}

/// Emite cada una de las instrucciones de una función no externa.
///
/// La correspondencia IR:ensamblador es siempre 1:N.
fn emit_body<'a, E: Emitter<'a>>(
    output: &'a mut dyn Write,
    function: &'a Function,
    instructions: &[Instruction],
) -> io::Result<()> {
    let locals = instructions
        .iter()
        .map(required_locals)
        .max()
        .unwrap_or(0)
        .max(function.parameters);

    // Colocar cada función en su propia sección permite eliminar
    // código muerto con -Wl,--gc-sections en la fase de enlazado
    writeln!(output, ".section .text.{0}\n{0}:", function.name)?;

    let context = Context {
        function,
        output: RefCell::new(output),
        locals,
        frame_info: Default::default(),
    };

    let mut emitter = E::new(context, instructions)?;
    for instruction in instructions {
        use Instruction::*;

        match instruction {
            SetLabel(Label(label)) => {
                writeln!(emitter.cx(), "\t.L{}.{}:", function.name, label)?;
            }

            Jump(label) => {
                let label = label_symbol(function, *label);
                emitter.jump_unconditional(&label)?;
            }

            JumpIfFalse(local, label) => {
                let label = label_symbol(function, *label);
                emitter.jump_if_false(*local, &label)?;
            }

            LoadConst(value, local) => emitter.load_const(*value, *local)?,
            LoadGlobal(global, local) => emitter.load_global(global, *local)?,
            StoreGlobal(local, global) => emitter.store_global(*local, global)?,

            Call {
                target,
                arguments,
                output,
            } => {
                emitter.call(&target, &arguments, *output)?;
            }
        }
    }

    emitter.epilogue()
}

/// Genera el símbolo que corresponde a una etiqueta dentro de una función.
fn label_symbol(function: &Function, Label(label): Label) -> String {
    format!(".L{}.{}", function.name, label)
}

/// Cuenta la mínima cantidad de locales que una instrucción exige
/// que se encuentren disponsibles.
fn required_locals(instruction: &Instruction) -> u32 {
    use Instruction::*;

    let required = |Local(local)| local + 1;
    match instruction {
        JumpIfFalse(local, _) => required(*local),
        LoadConst(_, local) => required(*local),
        LoadGlobal(_, local) => required(*local),
        StoreGlobal(local, _) => required(*local),

        Call {
            arguments, output, ..
        } => arguments
            .iter()
            .copied()
            .map(required)
            .max()
            .or(output.map(required))
            .unwrap_or(0),

        SetLabel(_) | Jump(_) => 0,
    }
}
