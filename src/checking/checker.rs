//! Contains code for the semantic analysis of a till AST and its conversion to
//! a final immediate representation of the input program.

use crate::{ stream, parsing };
use std::collections::HashMap;

pub fn input<T: Iterator<Item=parsing::Statement>>(stmts: T) -> super::Result<Vec<super::Instruction>> {
    Checker::new(stmts).execute()
}

/// Performs scoping and type checking on a stream of parsed statements. Yields
/// a final lower-level immediate representation of the input program.
pub struct Checker<T: Iterator<Item=parsing::Statement>> {
    /// Iterator of statements to be checked.
    stmts: T,
    /// Contains all variables declared outside of any function.
    global_variables: HashMap<super::Id, super::VariableDef>,
    /// Contains all function definitions.
    functions: Vec<super::FunctionDef>,
    /// The scope stack. The scope at the end of this vector is the inner most
    /// scope at a given point.
    scopes: Vec<super::Scope>,
    /// Holds the primitive instructions that will make up the final immediate
    /// representation of the input program.
    final_ir: Vec<super::Instruction>,
    /// Counter for creating unique IDs.
    id_counter: super::Id
}

impl<T: Iterator<Item=parsing::Statement>> Checker<T> {
    fn new(stmts: T) -> Self {
        Checker {
            stmts,
            global_variables: HashMap::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            final_ir: Vec::new(),
            id_counter: 0
        }
    }

    /// Perform scoping and type checking before yielding the final immediate
    /// representation of the input program. This will consume the `Checker`
    /// instance.
    fn execute(mut self) -> super::Result<Vec<super::Instruction>> {
        // Evaluate top-level statements:
        while let Some(stmt) = self.stmts.next() {
            self.eval_top_level_stmt(stmt)?;
        }

        assert!(self.scopes.is_empty());

        Ok(self.final_ir)
    }

    /// Ensure the validity and evaluate a top-level statement (function
    /// definition expected).
    fn eval_top_level_stmt(&mut self, stmt: parsing::Statement) -> super::Result<()> {
        match stmt {
            parsing::Statement::FunctionDefinition { pos, identifier, parameters, return_type, body } => {
                // Add the function instruction before the function body:
                let id = self.new_id();
                self.final_ir.push(super::Instruction::Function(id));

                // Check the declared return type is actually a real type:
                let checked_return_type = return_type.map(|x| super::Type::from_identifier(&x)).transpose()?;

                let mut param_types = Vec::new();
                for param in parameters.iter() {
                    param_types.push(super::Type::from_identifier(&param.param_type)?);
                }
                let checked_parameters = parameters.into_iter().map(|x| x.identifier).zip(param_types.clone().into_iter()).collect();

                // Check if the function already exists:
                if self.function_lookup(&identifier, param_types.as_slice(), &pos).is_ok() {
                    return Err(super::Failure::RedefinedExistingFunction(identifier.to_string(), param_types.to_vec()))
                }
                else {
                    // Create the function definition before evaluating the body
                    // so as to allow recursion:
                    self.add_function_def(identifier.clone(), param_types.clone(), checked_return_type.clone(), id);
                }

                // Evaluate the function body:
                let optional_body_return_type = self.eval_block(body, checked_parameters)?;

                // Return type specified in function signature:
                if let Some(expected_return_type) = checked_return_type {
                    // Function body should return something if a return type
                    // has been specified in the signature:
                    if let Some(body_return_type) = optional_body_return_type {
                        // Are those types the same?
                        if body_return_type == expected_return_type { Ok(()) }
                        else {
                            Err(super::Failure::FunctionUnexpectedReturnType {
                                pos, identifier, params: param_types.to_vec(),
                                expected: expected_return_type,
                                encountered: Some(body_return_type)
                            })
                        }
                    }
                    else {
                        return Err(super::Failure::FunctionUnexpectedReturnType {
                            pos, identifier, params: param_types.to_vec(),
                            expected: expected_return_type, encountered: None
                        });
                    }
                } // No return type specified in signature:
                else {
                    // Does function body return something?
                    if let Some(body_return_type) = optional_body_return_type {
                        Err(super::Failure::VoidFunctionReturnsValue(
                            pos, identifier, param_types.to_vec(),
                            body_return_type
                        ))
                    }
                    else { Ok(()) }
                }
            }

            parsing::Statement::VariableDeclaration {var_type, identifier, value } => {
                unimplemented!() // TODO: Globals...
            }

            _ => Err(super::Failure::InvalidTopLevelStatement)
        }
    }

    /// Check the validity of a given statement within a function. May return a
    /// type and stream  position in the case of the statement being a return
    /// statement or a while or if statement with a block containing a return
    /// statement.
    fn eval_inner_stmt(&mut self, stmt: parsing::Statement) -> super::Result<Option<(super::Type, stream::Position)>> {
        match stmt {
            parsing::Statement::Return(Some(expr)) => {
                let (ret_type, pos) = self.eval_expr(expr)?;
                self.final_ir.push(super::Instruction::ReturnValue);
                Ok(Some((ret_type, pos)))
            }
            parsing::Statement::Return(None) => {
                self.final_ir.push(super::Instruction::ReturnVoid);
                Ok(None)
            }

            parsing::Statement::Display(expr) => {
                let (value_type, pos) = self.eval_expr(expr)?;
                self.final_ir.push(super::Instruction::Display {
                    value_type, line_number: pos.line_number
                });
                Ok(None)
            }

            parsing::Statement::While { condition, block } => {
                let block_end_id = self.new_id();
                self.final_ir.push(super::Instruction::Jump(block_end_id));

                let start_id = self.new_id();
                self.final_ir.push(super::Instruction::Label(start_id));

                let block_ret_type = self.eval_block(block, vec![])?;
                self.final_ir.push(super::Instruction::Label(block_end_id));

                let pos = self.expect_expr_type(condition, super::Type::Bool)?;
                self.final_ir.push(super::Instruction::JumpIfTrue(start_id));

                Ok(
                    if let Some(ret_type) = block_ret_type { Some((ret_type, pos)) }
                    else { None }
                )
            }

            parsing::Statement::If { condition, block } => {
                let skip_block_id = self.new_id();

                let pos = self.expect_expr_type(condition, super::Type::Bool)?;
                self.final_ir.push(super::Instruction::JumpIfFalse(skip_block_id));

                let block_ret_type = self.eval_block(block, vec![])?;

                self.final_ir.push(super::Instruction::Label(skip_block_id));

                Ok(
                    if let Some(ret_type) = block_ret_type { Some((ret_type, pos)) }
                    else { None }
                )
            }

            parsing::Statement::VariableDeclaration { var_type, identifier, value } => {
                let checked_type = super::Type::from_identifier(&var_type)?;

                let var_id = {
                    // If variable is already defined in this same scope then
                    // ensure it is being redeclared to the same type:
                    if let Some(existing_def) = self.get_inner_scope().find_variable_def(&identifier) {
                        log::trace!("Redeclaring variable '{}' in same scope", identifier);

                        if checked_type != existing_def.var_type {
                            return Err(
                                super::Failure::VariableRedeclaredToDifferentType {
                                    identifier: identifier.to_string(),
                                    expected: existing_def.var_type.clone(),
                                    encountered: checked_type
                                }
                            );
                        }

                        existing_def.id
                    }
                    else {
                        log::trace!("Introducing variable '{}' to current scope", identifier);

                        let id = self.add_variable_def_to_inner_scope(identifier, checked_type.clone(), value.is_some());
                        self.final_ir.push(super::Instruction::Local(id));

                        id
                    }
                };

                // Ensure initial value expression is of correct type:
                if let Some(initial_value) = value {
                    self.expect_expr_type(initial_value, checked_type)?;

                    // Store the initial value instruction:
                    self.final_ir.push(super::Instruction::Store(var_id));
                }

                Ok(None)
            }

            parsing::Statement::VariableAssignment { identifier, assign_to } => {
                let var_id = {
                    let (assign_to_type, strm_pos) = self.eval_expr(assign_to)?;
                    
                    let var_def = self.variable_lookup(&identifier, &strm_pos)?;

                    if var_def.var_type != assign_to_type {
                        return Err(super::Failure::UnexpectedType {
                            pos: strm_pos,
                            encountered: assign_to_type,
                            expected: var_def.var_type.clone()
                        });
                    }

                    var_def.id
                };

                self.final_ir.push(super::Instruction::Store(var_id));

                Ok(None)
            }

            parsing::Statement::FunctionDefinition { pos, identifier, parameters: _, return_type: _, body: _ } =>
                Err(super::Failure::NestedFunctions(pos, identifier))
        }
    }

    /// Iterate over the statements contained in a block, checking each. Should
    /// a return statement be encountered, the type of the returned expression
    /// is returned within `Ok(Some(...))`. If there are multiple return statements
    /// then it will be ensured that they are all returning the same type.
    fn eval_block(&mut self, block: parsing::Block, params: Vec<(String, super::Type)>) -> super::Result<Option<super::Type>> {
        let mut ret_type = None;

        self.begin_new_scope();

        for (identifier, param_type) in params.into_iter().rev() {
            let var_id = self.add_variable_def_to_inner_scope(identifier, param_type, true);
            self.final_ir.push(super::Instruction::Parameter(var_id));
        }

        for stmt in block {
            if let Some((new, pos)) = self.eval_inner_stmt(stmt)? {
                // Has a return type already been established for this block?
                if let Some(current) = &ret_type {
                    if new != *current { // Can't have return statements with different types!
                        return Err(super::Failure::UnexpectedType {
                            pos, expected: current.clone(),
                            encountered: new
                        })
                    }
                }
                else { ret_type.replace(new); }
            }
        }

        self.end_scope();

        Ok(ret_type)
    }

    /// Introduce a new, inner-most scope which is added to the end of the scope
    /// stack.
    fn begin_new_scope(&mut self) {
        self.scopes.push(super::Scope { variables: Vec::new() });
    }

    /// Remove the inner-most scope from the scopes stack.
    fn end_scope(&mut self) {
        self.scopes.pop();
    }

    /// Get a mutable reference to the current inner-most scope. Will panic if
    /// the scope stack is empty.
    fn get_inner_scope(&mut self) -> &mut super::Scope {
        self.scopes.last_mut().unwrap()
    }

    /// Search for a definition for a function with a given identifier and set
    /// of parameter types.
    fn function_lookup(&self, ident: &str, params: &[super::Type], strm_pos: &stream::Position) -> super::Result<&super::FunctionDef> {
        for def in self.functions.iter() {
            if def.identifier == ident && def.parameter_types == params {
                return Ok(def);
            }
        }
        Err(super::Failure::FunctionUndefined(strm_pos.clone(), ident.to_string(), params.to_vec()))
    }

    fn add_function_def(&mut self, identifier: String, parameter_types: Vec<super::Type>, return_type: Option<super::Type>, id: super::Id) {
        self.functions.push(super::FunctionDef {
            identifier, parameter_types, return_type, id
        });
    }

    /// Search the current accessible scopes for the variable definition with
    /// the given identifier.
    fn variable_lookup(&self, ident: &str, strm_pos: &stream::Position) -> super::Result<&super::VariableDef> {
        // Reverse the iterator so that the inner most scope has priority (i.e.
        // automatically handle shadowing).
        for scope in self.scopes.iter().rev() {
            if let Some(var_def) = scope.find_variable_def(ident) {
                return Ok(var_def)
            }
        }
        Err(super::Failure::VariableNotInScope(strm_pos.clone(), ident.to_string()))
    }

    fn add_variable_def_to_inner_scope(&mut self, identifier: String, var_type: super::Type, initialised: bool) -> super::Id {
        let id = self.new_id();
        
        self.get_inner_scope().variables.push(super::VariableDef {
            identifier, var_type, initialised, id
        });
        
        id
    }

    /// Check the validity of a given expression as well as insert the appropriate
    /// instructions into the final IR.
    fn eval_expr(&mut self, expr: parsing::Expression) -> super::Result<(super::Type, stream::Position)> {
        match expr {
            parsing::Expression::Variable { pos, identifier } => {
                log::trace!("Searching scope for the type of referenced variable with identifier '{}'", identifier);

                let (var_type, id) = { // TODO: Check if variable is initialised before use!
                    let def = self.variable_lookup(&identifier, &pos)?;
                    (def.var_type.clone(), def.id)
                };

                self.final_ir.push(super::Instruction::Push(super::Value::Variable(id)));

                Ok((var_type, pos))
            }

            parsing::Expression::FunctionCall {pos, identifier, args } => {
                log::trace!("Searching scope for the return type of referenced function '{}' given arguments {:?}", identifier, args);

                let mut arg_types = Vec::new();
                for arg in args {
                    let (arg_type, _) = self.eval_expr(arg)?;
                    arg_types.push(arg_type); 
                }

                let (ident, option_ret_type, id) = {
                    let def = self.function_lookup(&identifier, arg_types.as_slice(), &pos)?;
                    (def.identifier.clone(), def.return_type.clone(), def.id)
                };

                self.final_ir.push(
                    if option_ret_type.is_some() { super::Instruction::CallExpectingValue(id) }
                    else { super::Instruction::CallExpectingVoid(id) }
                );

                match option_ret_type {
                    Some(ret_type) => Ok((ret_type.clone(), pos)),
                    None => Err(super::Failure::VoidFunctionInExpr(pos, ident, arg_types))
                }
            }

            parsing::Expression::Add(l, r) =>
                Ok((super::Type::Num, self.eval_arithmetic_expr(*l, *r, super::Instruction::Add, "addition")?)),

            parsing::Expression::Subtract(l, r) =>
                Ok((super::Type::Num, self.eval_arithmetic_expr(*l, *r, super::Instruction::Subtract, "subtraction")?)),

            parsing::Expression::Multiply(l, r) =>
                Ok((super::Type::Num, self.eval_arithmetic_expr(*l, *r, super::Instruction::Multiply, "multiplication")?)),

            parsing::Expression::Divide(l, r) =>
                Ok((super::Type::Num, self.eval_arithmetic_expr(*l, *r, super::Instruction::Divide, "divide")?)),

            parsing::Expression::GreaterThan(l, r) =>
                Ok((super::Type::Bool, self.eval_arithmetic_expr(*l, *r, super::Instruction::GreaterThan, "greater than")?)),

            parsing::Expression::LessThan(l, r) =>
                Ok((super::Type::Bool, self.eval_arithmetic_expr(*l, *r, super::Instruction::LessThan, "less than")?)),

            parsing::Expression::Equal(left, right) => {
                log::trace!("Verifying types of equality expression - types on both sides of the operator should be the same");

                let (left_type, strm_pos) = self.eval_expr(*left)?;
                let (right_type, _) = self.eval_expr(*right)?;

                if left_type == right_type {
                    self.final_ir.push(super::Instruction::Equals);

                    Ok((super::Type::Bool, strm_pos))
                }
                else {
                    Err(super::Failure::UnexpectedType {
                        pos: strm_pos,
                        expected: left_type,
                        encountered: right_type
                    })
                }
            }

            parsing::Expression::BooleanNot(expr) => {
                log::trace!("Verifying type of expression to which boolean NOT operator is being applied - expecting Bool expression to right of operator");

                let strm_pos = self.expect_expr_type(*expr, super::Type::Bool)?;
                self.final_ir.push(super::Instruction::Not);

                Ok((super::Type::Bool, strm_pos))
            }

            parsing::Expression::UnaryMinus(expr) => {
                log::trace!("Verify type of expression to which unary minus is being applied - expecting Num");

                self.final_ir.push(super::Instruction::Push(super::Value::Num(0.0)));
                let strm_pos = self.expect_expr_type(*expr, super::Type::Num)?;
                self.final_ir.push(super::Instruction::Subtract);

                Ok((super::Type::Num, strm_pos))
            }

            parsing::Expression::NumberLiteral {pos, value } => {
                self.final_ir.push(super::Instruction::Push(super::Value::Num(value)));

                Ok((super::Type::Num, pos))
            }
            parsing::Expression::BooleanLiteral { pos, value } => {
                self.final_ir.push(super::Instruction::Push(super::Value::Bool(value)));

                Ok((super::Type::Bool, pos))
            }
            parsing::Expression::CharLiteral { pos, value } => {
                self.final_ir.push(super::Instruction::Push(super::Value::Char(value)));

                Ok((super::Type::Char, pos))
            }
        }
    }

    /// Ensure the two sub-expressions of an arithmetic expression are both of
    /// Num type. Insert the relevant final IR instruction also.
    fn eval_arithmetic_expr(&mut self, left: parsing::Expression, right: parsing::Expression, instruction: super::Instruction, expr_type: &str) -> super::Result<stream::Position> {
        log::trace!("Verifying types of {} expression - Num type on both sides of operator expected", expr_type);

        let strm_pos = self.expect_expr_type(left, super::Type::Num)?;
        self.expect_expr_type(right, super::Type::Num)?;

        self.final_ir.push(instruction);

        Ok(strm_pos)
    }

    fn expect_expr_type(&mut self, expr: parsing::Expression, expected: super::Type) -> super::Result<stream::Position> {
        let (expr_type, strm_pos) = self.eval_expr(expr)?;
        
        if expr_type == expected { Ok(strm_pos) }
        else {
            Err(super::Failure::UnexpectedType {
                pos: strm_pos, expected, encountered: expr_type
            }) 
        }
    }

    fn new_id(&mut self) -> super::Id {
        let id = self.id_counter;
        self.id_counter += 1;
        id
    }
}



#[cfg(test)]
mod tests {
    use std::iter;
    use crate::{ assert_pattern, parsing, checking, stream::Position };

    fn new_empty_checker() -> super::Checker<iter::Empty<parsing::Statement>> {
        let mut chkr = super::Checker::new(iter::empty());
        chkr.begin_new_scope();
        chkr
    }

    #[test]
    fn scoping() {
        let mut chkr = new_empty_checker();

        let pos = Position::new();

        chkr.add_variable_def_to_inner_scope("outer".to_string(), checking::Type::Num, false);
        assert_eq!(chkr.variable_lookup("outer", &pos), Ok(&checking::VariableDef {
            identifier: "outer".to_string(),
            var_type: checking::Type::Num,
            initialised: false,
            id: 0
        }));

        chkr.begin_new_scope();

        chkr.add_variable_def_to_inner_scope("inner".to_string(), checking::Type::Bool, false);

        assert!(chkr.variable_lookup("inner", &pos).is_ok());
        assert!(chkr.variable_lookup("outer", &pos).is_ok());

        chkr.end_scope();

        assert!(chkr.variable_lookup("inner", &pos).is_err());
        assert!(chkr.variable_lookup("outer", &pos).is_ok());
        assert!(chkr.variable_lookup("undefined", &pos).is_err());
    }

    #[test]
    fn eval_exprs() {
        let mut chkr = new_empty_checker();

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::NumberLiteral { pos: Position::new(), value: 10.5 }),
            Ok((checking::Type::Num, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::BooleanLiteral { pos: Position::new(), value: true }),
            Ok((checking::Type::Bool, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::CharLiteral { pos: Position::new(), value: '話' }),
            Ok((checking::Type::Char, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Equal(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }),
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'y' })
            )),
            Ok((checking::Type::Bool, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Equal(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.5 }),
                Box::new(parsing::Expression::BooleanLiteral { pos: Position::new(), value: false })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Bool,
                expected: checking::Type::Num, pos: _
            })
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::GreaterThan(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.34 }),
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 0.95 })
            )),
            Ok((checking::Type::Bool, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::LessThan(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'b' }),
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'a' })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Char,
                expected: checking::Type::Num, pos: _
            })
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Add(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 10.0 }),
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 11.2 })
            )),
            Ok((checking::Type::Num, _))
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Divide(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }),
                Box::new(parsing::Expression::BooleanLiteral { pos: Position::new(), value: false })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Char,
                expected: checking::Type::Num, pos: _
            })
        );

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Variable {
                pos: Position::new(),
                identifier: "undefined".to_string()
            }),
            Err(checking::Failure::VariableNotInScope(_, _))
        );

        chkr.add_variable_def_to_inner_scope("var".to_string(), checking::Type::Num, true);

        chkr.begin_new_scope();
        assert_pattern!(
            chkr.eval_expr(parsing::Expression::Variable {
                pos: Position::new(),
                identifier: "var".to_string()
            }),
            Ok((checking::Type::Num, _))
        );
        chkr.end_scope();

        chkr.add_function_def("func".to_string(), vec![], Some(checking::Type::Num), 0);

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::FunctionCall {
                pos: Position::new(),
                identifier: "func".to_string(),
                args: vec![]
            }),
            Ok((checking::Type::Num, _))
        );

        match chkr.eval_expr(parsing::Expression::FunctionCall {
            pos: Position::new(),
            identifier: "func".to_string(),
            args: vec![
                parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.5 }
            ]
        }) {
            Err(checking::Failure::FunctionUndefined(_, ident, args)) => {
                assert_eq!(ident, "func".to_string());
                assert_eq!(args, vec![checking::Type::Num]);
            }
            _ => panic!()
        }

        chkr.add_function_def("abc".to_string(), vec![checking::Type::Char], None, 1);

        assert_pattern!(
            chkr.eval_expr(parsing::Expression::FunctionCall {
                pos: Position::new(),
                identifier: "abc".to_string(),
                args: vec![
                    parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }
                ]
            }),
            Err(checking::Failure::VoidFunctionInExpr(_, _, _))
        );
    }

    #[test]
    fn eval_inner_stmts() {
        let mut chkr = new_empty_checker();

        assert_eq!(
            chkr.eval_inner_stmt(parsing::Statement::Return(None)),
            Ok(None)
        );

        assert_pattern!(
            chkr.eval_inner_stmt(parsing::Statement::Return(Some(
                parsing::Expression::Add(
                    Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.2 }),
                    Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 2.8 })
                )
            ))),
            Ok(Some((checking::Type::Num, _)))
        );

        assert_pattern!(
            chkr.eval_inner_stmt(parsing::Statement::If {
                condition: parsing::Expression::BooleanLiteral { pos: Position::new(), value: true },
                block: vec![
                    parsing::Statement::Return(Some(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }))
                ]
            }),
            Ok(Some((checking::Type::Char, _)))
        );

        assert_eq!(
            chkr.eval_inner_stmt(parsing::Statement::VariableDeclaration {
                identifier: "pi".to_string(),
                var_type: "Num".to_string(),
                value: Some(parsing::Expression::NumberLiteral { pos: Position::new(), value: 3.14 })
            }),
            Ok(None)
        );
        assert!(chkr.variable_lookup("pi", &Position::new()).is_ok());

        assert_eq!(
            chkr.eval_inner_stmt(parsing::Statement::VariableDeclaration {
                identifier: "xyz".to_string(),
                var_type: "Oops".to_string(),
                value: None
            }),
            Err(checking::Failure::NonexistentPrimitiveType("Oops".to_string()))
        );

        assert_eq!(
            chkr.eval_inner_stmt(parsing::Statement::VariableAssignment {
                identifier: "pi".to_string(),
                assign_to: parsing::Expression::NumberLiteral { pos: Position::new(), value: 3.1 }
            }),
            Ok(None)
        );

        assert_pattern!(
            chkr.eval_inner_stmt(parsing::Statement::VariableAssignment {
                identifier: "pi".to_string(),
                assign_to: parsing::Expression::BooleanLiteral { pos: Position::new(), value: true }
            }),
            Err(checking::Failure::UnexpectedType {
                expected: checking::Type::Num,
                encountered: checking::Type::Bool, pos: _
            })
        );

        assert_pattern!(
            chkr.eval_inner_stmt(parsing::Statement::FunctionDefinition {
                identifier: "nested".to_string(),
                parameters: vec![],
                return_type: None,
                body: vec![],
                pos: Position::new()
            }),
            Err(checking::Failure::NestedFunctions(_, _))
        );
    }

    #[test]
    fn eval_top_level_stmts() -> checking::Result<()> {
        let mut chkr = new_empty_checker();

        assert_eq!(
            chkr.eval_top_level_stmt(parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![],
                return_type: None,
                body: vec![],
                pos: Position::new()
            }),
            Ok(())
        );
        assert!(chkr.function_lookup("func", &[], &Position::new())?.return_type.is_none());

        assert_eq!(
            chkr.eval_top_level_stmt(parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![],
                return_type: Some("Num".to_string()),
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::NumberLiteral {
                        pos: Position::new(), value: 1.5
                    }))
                ],
                pos: Position::new()
            }),
            Err(checking::Failure::RedefinedExistingFunction(
                "func".to_string(), vec![]
            ))
        );

        assert_pattern!(
            chkr.eval_top_level_stmt(parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![
                    parsing::Parameter {
                        pos: Position::new(), identifier: "x".to_string(),
                        param_type: "Char".to_string()
                    }
                ],
                return_type: Some("Num".to_string()),
                body: vec![],
                pos: Position::new()
            }),
            Err(checking::Failure::FunctionUnexpectedReturnType {
                pos: _, identifier: _, params: _,
                expected: checking::Type::Num, encountered: None
            })
        );

        assert_pattern!(
            chkr.eval_top_level_stmt(parsing::Statement::FunctionDefinition {
                identifier: "xyz".to_string(),
                parameters: vec![],
                return_type: None,
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::BooleanLiteral {
                        pos: Position::new(), value: true
                    }))
                ],
                pos: Position::new()
            }),
            Err(checking::Failure::VoidFunctionReturnsValue(
                _, _, _, checking::Type::Bool
            ))
        );

        assert_eq!(
            chkr.eval_top_level_stmt(parsing::Statement::FunctionDefinition {
                identifier: "useless_function".to_string(),
                parameters: vec![
                    parsing::Parameter {
                        pos: Position::new(), identifier: "x".to_string(),
                        param_type: "Num".to_string()
                    }
                ],
                return_type: Some("Num".to_string()),
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::Variable {
                        pos: Position::new(), identifier: "x".to_string()
                    }))
                ],
                pos: Position::new()
            }),
            Ok(())
        );

        Ok(())
    }

    #[test]
    fn variable_shadowing() -> checking::Result<()> {
        let mut chkr = new_empty_checker();

        let pos = Position::new();

        chkr.eval_inner_stmt(parsing::Statement::VariableDeclaration {
            identifier: "x".to_string(),
            var_type: "Num".to_string(),
            value: None
        })?;

        chkr.begin_new_scope();

        // Shadow variable 'x' by declaring a variable in the inner scope of the
        // same name but a different type:
        chkr.eval_inner_stmt(parsing::Statement::VariableDeclaration {
            identifier: "x".to_string(),
            var_type: "Bool".to_string(),
            value: None
        })?;

        assert_eq!(chkr.variable_lookup("x", &pos)?.var_type, checking::Type::Bool);

        chkr.end_scope();

        assert_eq!(chkr.variable_lookup("x", &pos)?.var_type, checking::Type::Num);

        assert_eq!(
            chkr.eval_inner_stmt(parsing::Statement::VariableDeclaration {
                identifier: "x".to_string(),
                var_type: "Char".to_string(),
                value: None
            }),
            Err(checking::Failure::VariableRedeclaredToDifferentType {
                identifier: "x".to_string(),
                expected: checking::Type::Num,
                encountered: checking::Type::Char
            })
        );

        Ok(())
    }
}