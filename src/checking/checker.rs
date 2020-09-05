//! Contains code for the semantic analysis of a till AST and its conversion to
//! a final immediate representation of the input program.

use crate::parsing;

pub fn input<T: Iterator<Item=parsing::Statement>>(stmts: T) -> super::Result<Vec<super::Instruction>> {
    Checker::new(stmts).execute()
}

/// Performs scoping and type checking on a stream of parsed statements. Yields
/// a final lower-level immediate representation of the input program.
pub struct Checker<T: Iterator<Item=parsing::Statement>> {
    /// Iterator of statements to be checked.
    stmts: T,
    /// The scope stack. The scope at the end of this vector is the inner most
    /// scope at a given point.
    scopes: Vec<super::Scope>,
    /// Holds the primitive instructions that will make up the final immediate
    /// representation of the input program.
    final_ir: Vec<super::Instruction>,
    /// Counter for creating unique IDs for variables. Incremented each time a
    /// new variable is created.
    var_id_counter: super::VarId
}

impl<T: Iterator<Item=parsing::Statement>> Checker<T> {
    fn new(stmts: T) -> Self {
        Checker {
            stmts,
            scopes: Vec::new(),
            final_ir: Vec::new(),
            var_id_counter: 0
        }
    }

    /// Perform scoping and type checking before yielding the final immediate
    /// representation of the input program. This will consume the `Checker`
    /// instance.
    fn execute(mut self) -> super::Result<Vec<super::Instruction>> {
        self.begin_new_scope();
    
        while let Some(stmt) = self.stmts.next() {
            self.check_stmt(&stmt)?;
        }

        log::info!("Reached end of statement stream - ending program scope");

        self.end_scope();
        assert!(self.scopes.is_empty());

        Ok(self.final_ir)
    }

    /// Check the validity of a given statement. May return a type in the case of
    /// the statement being a return statement.
    fn check_stmt(&mut self, stmt: &parsing::Statement) -> super::Result<Option<super::Type>> {
        match stmt {
            parsing::Statement::Return(Some(expr)) => {
                let value = self.check_expr(expr)?;
                self.final_ir.push(super::Instruction::ReturnValue);
                Ok(Some(value))
            }
            parsing::Statement::Return(None) => {
                self.final_ir.push(super::Instruction::ReturnVoid);
                Ok(None)
            }

            parsing::Statement::If { condition, block } |
            parsing::Statement::While { condition, block } => {
                // TODO: Final IR instructions
                self.expect_expr_type(condition, super::Type::Bool)?;
                let (block_ret_type, _) = self.check_block(block, &Vec::new())?;
                Ok(block_ret_type)
            }

            parsing::Statement::VariableDeclaration { var_type, identifier, value } => {
                let checking_type = super::Type::from_parsing_type(var_type)?;

                let var_id = { 
                    // If variable is already defined then ensure it is being redeclared
                    // to the same type:
                    if let Some(existing_def) = self.get_inner_scope().find_variable_def(identifier) {
                        log::trace!("Redeclaring variable '{}' in same scope", identifier);

                        if checking_type != existing_def.var_type {
                            return Err(super::Failure::RedeclaredToDifferentType {
                                identifier: identifier.to_string(),
                                expected: existing_def.var_type.clone(),
                                encountered: checking_type
                            });
                        }

                        existing_def.id
                    }
                    else {
                        log::trace!("Introducing variable '{}' to current scope", identifier);

                        let var_id = self.introduce_variable_to_inner_scope(identifier, checking_type.clone());
                        self.final_ir.push(super::Instruction::Allocate(var_id));

                        var_id
                    }
                };

                // Ensure initial value expression is of correct type:
                if let Some(initial_value) = value {
                    self.expect_expr_type(initial_value, checking_type)?;

                    // Store the initial value instruction:
                    self.final_ir.push(super::Instruction::Store(var_id));
                }

                Ok(None)
            }

            parsing::Statement::VariableAssignment { identifier, assign_to } => {
                let var_id = {
                    let assign_to_type = self.check_expr(assign_to)?;
                    
                    let var_def = self.variable_lookup(identifier)?;

                    if var_def.var_type != assign_to_type {
                        return Err(super::Failure::UnexpectedType {
                            encountered: assign_to_type,
                            expected: var_def.var_type.clone()
                        });
                    }

                    var_def.id
                };

                self.final_ir.push(super::Instruction::Store(var_id));

                Ok(None)
            }

            parsing::Statement::FunctionDefinition { identifier, parameters, return_type, body } => {
                let (optional_body_return_type, param_types) = self.check_block(body, parameters)?;

                // Is a function with the same identifier and type signature
                // defined and accessible from this scope?
                if self.function_lookup(identifier, param_types.as_slice()).is_ok() {
                    Err(super::Failure::RedefinedExistingFunction(identifier.to_string(), param_types.to_vec()))
                }
                else {
                    // Return type specified in function signature:
                    if let Some(parsing_return_type) = return_type {
                        let expected_return_type = super::Type::from_parsing_type(parsing_return_type)?;

                        // Function body should return something if a return type
                        // has been specified in the signature:
                        if let Some(body_return_type) = optional_body_return_type {
                            // Are those types the same?
                            if body_return_type == expected_return_type {
                                self.introduce_function(identifier, param_types.as_slice(), Some(body_return_type));
                                Ok(None)
                            }
                            else {
                                Err(super::Failure::UnexpectedType {
                                    encountered: body_return_type,
                                    expected: expected_return_type
                                })
                            }
                        }
                        else {
                            return Err(super::Failure::FunctionDoesNotReturn(
                                identifier.to_string(), param_types.to_vec(),
                                expected_return_type
                            ));
                        }
                    } // No return type specified in signature:
                    else {
                        // Does function body return something?
                        if let Some(body_return_type) = optional_body_return_type {
                            Err(super::Failure::VoidFunctionReturnsValue(
                                identifier.to_string(), param_types.to_vec(),
                                body_return_type
                            ))
                        }
                        else {
                            self.introduce_function(identifier, param_types.as_slice(), None);
                            Ok(None)
                        }
                    }
                }
            }
        }
    }

    /// Iterate over the statements contained in a block, checking each. Should
    /// a return statement be encountered, the type of the returned expression
    /// is returned within `Ok(Some(...), ...)`. If there are multiple return
    /// statements then it will be ensured that they are all returning the same type.
    fn check_block(&mut self, block: &parsing::Block, params: &Vec<parsing::Parameter>) -> super::Result<(Option<super::Type>, Vec<super::Type>)> {
        let mut ret_type = None;

        self.begin_new_scope();

        let mut param_types = Vec::new();
        for param in params {
            let converted_type = super::Type::from_parsing_type(&param.param_type)?;
            self.introduce_variable_to_inner_scope(&param.identifier, converted_type.clone());
            param_types.push(converted_type);
        }

        for stmt in block {
            if let Some(new) = self.check_stmt(stmt)? {
                // Has a return type already been established for this block?
                if let Some(current) = &ret_type {
                    if new != *current { // Can't have return statements with different types!
                        return Err(super::Failure::UnexpectedType {
                            expected: current.clone(),
                            encountered: new
                        })
                    }
                }
                else { ret_type.replace(new); }
            }
        }

        self.end_scope();

        Ok((ret_type, param_types))
    }

    fn begin_new_scope(&mut self) {
        self.scopes.push(super::Scope {
            variable_defs: Vec::new(),
            function_defs: Vec::new()
        });
    }

    fn end_scope(&mut self) {
        if let Some(prev_scope) = self.scopes.pop() { // Remove scope from stack.
            // Deallocate all variables belonging to that stack:
            for var_def in prev_scope.variable_defs {
                self.final_ir.push(super::Instruction::Deallocate(var_def.id));
            }
        }
    }

    fn get_inner_scope(&mut self) -> &mut super::Scope {
        self.scopes.last_mut().unwrap()
    }

    /// Search the current accessible scopes for the variable definition with
    /// the given identifier.
    fn variable_lookup(&self, ident: &str) -> super::Result<&super::VariableDef> {
        // Reverse the iterator so that the inner most scope has priority (i.e.
        // automatically handle shadowing).
        for scope in self.scopes.iter().rev() {
            if let Some(var_def) = scope.find_variable_def(ident) {
                return Ok(var_def)
            }
        }
        Err(super::Failure::VariableNotInScope(ident.to_string()))
    }

    /// Introduce a new variable into the current inner most scope.
    fn introduce_variable_to_inner_scope(&mut self, ident: &str, var_type: super::Type) -> super::VarId {
        let id = self.var_id_counter;
        
        self.get_inner_scope().variable_defs.push(super::VariableDef {
            var_type, identifier: ident.to_string(), id
        });
        
        self.var_id_counter += 1;

        id
    }

    fn function_lookup(&self, ident: &str, params: &[super::Type]) -> super::Result<&super::FunctionDef> {
        for scope in self.scopes.iter().rev() {
            if let Some(func_def) = scope.find_function_def(ident, params) {
                return Ok(func_def)
            }
        }
        Err(super::Failure::FunctionNotInScope(ident.to_string(), params.to_vec()))
    }

    fn introduce_function(&mut self, ident: &str, params: &[super::Type], return_type: Option<super::Type>) {
        self.get_inner_scope().function_defs.push(super::FunctionDef {
            identifier: ident.to_string(),
            parameter_types: params.to_vec(),
            return_type
        })
    }

    fn check_expr(&mut self, expr: &parsing::Expression) -> super::Result<super::Type> {
        match expr {
            parsing::Expression::Variable { pos: _, identifier } => {
                log::trace!("Searching scope for the type of referenced variable with identifier '{}'", identifier);

                let (var_type, id) = {
                    let def = self.variable_lookup(identifier)?;
                    (def.var_type.clone(), def.id)
                };

                self.final_ir.push(super::Instruction::Push(super::Value::Variable(id)));

                Ok(var_type)
            }

            parsing::Expression::FunctionCall {pos: _, identifier, args } => {
                log::trace!("Searching scope for the return type of referenced function '{}' given arguments {:?}", identifier, args);

                // TODO: Produce final IR instructions

                let mut arg_types = Vec::new();
                for arg in args { arg_types.push(self.check_expr(arg)?) }

                let definition = self.function_lookup(identifier, arg_types.as_slice())?;
                
                match &definition.return_type {
                    Some(return_type) => Ok(return_type.clone()),
                    None => Err(super::Failure::VoidFunctionInExpr(identifier.to_string(), arg_types))
                }
            }

            parsing::Expression::Add(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::Add, "addition")?;
                Ok(super::Type::Num)
            }

            parsing::Expression::Subtract(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::Subtract, "subtraction")?;
                Ok(super::Type::Num)
            }

            parsing::Expression::Multiply(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::Multiply, "multiplication")?;
                Ok(super::Type::Num)
            }

            parsing::Expression::Divide(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::Divide, "divide")?;
                Ok(super::Type::Num)
            }

            parsing::Expression::GreaterThan(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::GreaterThan, "greater than")?;
                Ok(super::Type::Bool)
            }

            parsing::Expression::LessThan(l, r) => {
                self.verify_arithmetic_expr(l, r, super::Instruction::LessThan, "less than")?;
                Ok(super::Type::Bool)
            }

            parsing::Expression::Equal(left, right) => {
                log::trace!("Verifying types of equality expression - types on both sides of the operator should be the same");

                let left_type = self.check_expr(left)?;
                let right_type = self.check_expr(right)?;

                if left_type == right_type {
                    self.final_ir.push(super::Instruction::Equals);

                    Ok(super::Type::Bool)
                }
                else {
                    Err(super::Failure::UnexpectedType {
                        expected: left_type,
                        encountered: right_type
                    })
                }
            }

            parsing::Expression::BooleanNot(expr) => {
                log::trace!("Verifying type of expression to which boolean NOT operator is being applied - expecting Bool expression to right of operator");

                self.expect_expr_type(expr, super::Type::Bool)?;
                Ok(super::Type::Bool)
            }

            parsing::Expression::UnaryMinus(expr) => {
                self.expect_expr_type(expr, super::Type::Num)?;
                Ok(super::Type::Num)
            }

            parsing::Expression::Array(exprs) => {
                let contained_type = 
                    if exprs.is_empty() { super::Type::Any } // can't infer type of empty literal...
                    else { self.check_expr(&exprs[0])? };

                for expr in exprs {
                    let expr_type = self.check_expr(expr)?;

                    if contained_type != expr_type {
                        return Err(super::Failure::UnexpectedType {
                            expected: contained_type,
                            encountered: expr_type
                        })
                    }
                }

                Ok(super::Type::Array(Box::new(contained_type)))
            }

            parsing::Expression::StringLiteral { pos: _, value } => {
                // TODO: ...
                Ok(super::Type::Array(Box::new(super::Type::Char)))
            }
            parsing::Expression::NumberLiteral {pos: _, value } => {
                self.final_ir.push(super::Instruction::Push(
                    super::Value::Constant(super::ConstValue::Num(*value))
                ));

                Ok(super::Type::Num)
            }
            parsing::Expression::BooleanLiteral { pos: _, value } => {
                self.final_ir.push(super::Instruction::Push(
                    super::Value::Constant(super::ConstValue::Bool(*value))
                ));

                Ok(super::Type::Bool)
            }
            parsing::Expression::CharLiteral { pos: _, value } => {
                self.final_ir.push(super::Instruction::Push(
                    super::Value::Constant(super::ConstValue::Char(*value))
                ));

                Ok(super::Type::Char)
            }
        }
    }

    fn verify_arithmetic_expr(&mut self, left: &parsing::Expression, right: &parsing::Expression, instruction: super::Instruction, expr_type: &str) -> super::Result<()> {
        log::trace!("Verifying types of {} expression - Num type on both sides of operator expected", expr_type);

        self.expect_expr_type(left, super::Type::Num)?;
        self.expect_expr_type(right, super::Type::Num)?;

        self.final_ir.push(instruction);

        Ok(())
    }

    fn expect_expr_type(&mut self, expr: &parsing::Expression, expected: super::Type) -> super::Result<()> {
        let expr_type = self.check_expr(expr)?;
        
        if expr_type == expected { Ok(()) }
        else { Err(super::Failure::UnexpectedType { expected, encountered: expr_type }) }
    }
}



#[cfg(test)]
mod tests {
    use std::iter;
    use crate::{ parsing, checking, stream::Position };

    fn new_empty_checker() -> super::Checker<iter::Empty<parsing::Statement>> {
        let mut chkr = super::Checker::new(iter::empty());
        chkr.begin_new_scope();
        chkr
    }

    #[test]
    fn scoping() {
        let mut chkr = new_empty_checker();

        chkr.introduce_variable_to_inner_scope("outer", checking::Type::Num);
        assert_eq!(chkr.variable_lookup("outer"), Ok(&checking::VariableDef {
            identifier: "outer".to_string(),
            var_type: checking::Type::Num,
            id: 0
        }));

        chkr.begin_new_scope();

        chkr.introduce_variable_to_inner_scope("inner", checking::Type::Bool);

        assert!(chkr.variable_lookup("inner").is_ok());
        assert!(chkr.variable_lookup("outer").is_ok());

        chkr.end_scope();

        assert!(chkr.variable_lookup("inner").is_err());
        assert!(chkr.variable_lookup("outer").is_ok());
        assert!(chkr.variable_lookup("undefined").is_err());

        chkr.introduce_function("xyz", &[checking::Type::Char], Some(checking::Type::Num));
        
        assert_eq!(chkr.function_lookup("xyz", &[checking::Type::Char]), Ok(&checking::FunctionDef {
            identifier: "xyz".to_string(),
            parameter_types: vec![checking::Type::Char],
            return_type: Some(checking::Type::Num)
        }));

        assert!(chkr.function_lookup("xyz", &[checking::Type::Num]).is_err());
    }

    #[test]
    fn check_exprs() {
        let mut chkr = new_empty_checker();

        assert_eq!(
            chkr.check_expr(&parsing::Expression::NumberLiteral { pos: Position::new(), value: 10.5 }),
            Ok(checking::Type::Num)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::BooleanLiteral { pos: Position::new(), value: true }),
            Ok(checking::Type::Bool)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::CharLiteral { pos: Position::new(), value: '話' }),
            Ok(checking::Type::Char)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::StringLiteral { pos: Position::new(), value: "string".to_string() }),
            Ok(checking::Type::Array(Box::new(checking::Type::Char)))
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Array(vec![
                parsing::Expression::NumberLiteral { pos: Position::new(), value: 0.1 },
                parsing::Expression::NumberLiteral { pos: Position::new(), value: 0.2 }
            ])),
            Ok(checking::Type::Array(Box::new(checking::Type::Num)))
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Array(vec![
                parsing::Expression::CharLiteral { pos: Position::new(), value: 'a' },
                parsing::Expression::BooleanLiteral { pos: Position::new(), value: true }
            ])),
            Err(checking::Failure::UnexpectedType {
                expected: checking::Type::Char,
                encountered: checking::Type::Bool
            })
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Equal(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }),
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'y' })
            )),
            Ok(checking::Type::Bool)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Equal(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.5 }),
                Box::new(parsing::Expression::BooleanLiteral { pos: Position::new(), value: false })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Bool,
                expected: checking::Type::Num
            })
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::GreaterThan(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.34 }),
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 0.95 })
            )),
            Ok(checking::Type::Bool)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::LessThan(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'b' }),
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'a' })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Char,
                expected: checking::Type::Num
            })
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Add(
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 10.0 }),
                Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 11.2 })
            )),
            Ok(checking::Type::Num)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Divide(
                Box::new(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }),
                Box::new(parsing::Expression::BooleanLiteral { pos: Position::new(), value: false })
            )),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Char,
                expected: checking::Type::Num
            })
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::Variable {
                pos: Position::new(),
                identifier: "undefined".to_string()
            }),
            Err(checking::Failure::VariableNotInScope("undefined".to_string()))
        );

        chkr.introduce_variable_to_inner_scope("var", checking::Type::Num);

        chkr.begin_new_scope();
        assert_eq!(
            chkr.check_expr(&parsing::Expression::Variable {
                pos: Position::new(),
                identifier: "var".to_string()
            }),
            Ok(checking::Type::Num)
        );
        chkr.end_scope();

        chkr.introduce_function("func", &[], Some(checking::Type::Num));

        assert_eq!(
            chkr.check_expr(&parsing::Expression::FunctionCall {
                pos: Position::new(),
                identifier: "func".to_string(),
                args: vec![]
            }),
            Ok(checking::Type::Num)
        );

        assert_eq!(
            chkr.check_expr(&parsing::Expression::FunctionCall {
                pos: Position::new(),
                identifier: "func".to_string(),
                args: vec![
                    parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.5 }
                ]
            }),
            Err(checking::Failure::FunctionNotInScope("func".to_string(), vec![checking::Type::Num]))
        );

        chkr.introduce_function("abc", &[checking::Type::Char], None);

        assert_eq!(
            chkr.check_expr(&parsing::Expression::FunctionCall {
                pos: Position::new(),
                identifier: "abc".to_string(),
                args: vec![
                    parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }
                ]
            }),
            Err(checking::Failure::VoidFunctionInExpr("abc".to_string(), vec![checking::Type::Char]))
        );
    }

    #[test]
    fn check_stmts() -> checking::Result<()> {
        let mut chkr = new_empty_checker();

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::Return(None)),
            Ok(None)
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::Return(Some(
                parsing::Expression::Add(
                    Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.2 }),
                    Box::new(parsing::Expression::NumberLiteral { pos: Position::new(), value: 2.8 })
                )
            ))),
            Ok(Some(checking::Type::Num))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::If {
                condition: parsing::Expression::BooleanLiteral { pos: Position::new(), value: true },
                block: vec![
                    parsing::Statement::Return(Some(parsing::Expression::CharLiteral { pos: Position::new(), value: 'x' }))
                ]
            }),
            Ok(Some(checking::Type::Char))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::While {
                condition: parsing::Expression::StringLiteral { pos: Position::new(), value: "this isn't a bool!".to_string() },
                block: vec![]
            }),
            Err(checking::Failure::UnexpectedType {
                expected: checking::Type::Bool,
                encountered: checking::Type::Array(Box::new(checking::Type::Char))
            })
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "pi".to_string(),
                var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Num".to_string() },
                value: Some(parsing::Expression::NumberLiteral { pos: Position::new(), value: 3.14 })
            }),
            Ok(None)
        );
        assert!(chkr.variable_lookup("pi").is_ok());

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "abc".to_string(),
                var_type: parsing::Type::Array(Box::new(parsing::Type::Identifier { pos: Position::new(), identifier: "Num".to_string() })),
                value: Some(parsing::Expression::StringLiteral { pos: Position::new(), value: "this isn't a Num array!".to_string() })
            }),
            Err(checking::Failure::UnexpectedType {
                encountered: checking::Type::Array(Box::new(checking::Type::Char)),
                expected: checking::Type::Array(Box::new(checking::Type::Num))
            })
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "xyz".to_string(),
                var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Oops".to_string() },
                value: None
            }),
            Err(checking::Failure::NonexistentPrimitiveType("Oops".to_string()))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableAssignment {
                identifier: "pi".to_string(),
                assign_to: parsing::Expression::NumberLiteral { pos: Position::new(), value: 3.1 }
            }),
            Ok(None)
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableAssignment {
                identifier: "pi".to_string(),
                assign_to: parsing::Expression::BooleanLiteral { pos: Position::new(), value: true }
            }),
            Err(checking::Failure::UnexpectedType {
                expected: checking::Type::Num,
                encountered: checking::Type::Bool
            })
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![],
                return_type: None,
                body: vec![]
            }),
            Ok(None)
        );
        assert!(chkr.function_lookup("func", &[])?.return_type.is_none());

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![],
                return_type: Some(parsing::Type::Identifier {
                    pos: Position::new(), identifier: "Num".to_string()
                }),
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::NumberLiteral {
                        pos: Position::new(), value: 1.5
                    }))
                ]
            }),
            Err(checking::Failure::RedefinedExistingFunction(
                "func".to_string(), vec![]
            ))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::FunctionDefinition {
                identifier: "func".to_string(),
                parameters: vec![
                    parsing::Parameter {
                        pos: Position::new(), identifier: "x".to_string(),
                        param_type: parsing::Type::Identifier {
                            pos: Position::new(), identifier: "Char".to_string()
                        }
                    }
                ],
                return_type: Some(parsing::Type::Identifier {
                    pos: Position::new(), identifier: "Num".to_string()
                }),
                body: vec![]
            }),
            Err(checking::Failure::FunctionDoesNotReturn(
                "func".to_string(), vec![checking::Type::Char],
                checking::Type::Num
            ))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::FunctionDefinition {
                identifier: "xyz".to_string(),
                parameters: vec![],
                return_type: None,
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::BooleanLiteral {
                        pos: Position::new(), value: true
                    }))
                ]
            }),
            Err(checking::Failure::VoidFunctionReturnsValue(
                "xyz".to_string(), vec![], checking::Type::Bool
            ))
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::FunctionDefinition {
                identifier: "useless_function".to_string(),
                parameters: vec![
                    parsing::Parameter {
                        pos: Position::new(), identifier: "x".to_string(),
                        param_type: parsing::Type::Identifier {
                            pos: Position::new(), identifier: "Num".to_string()
                        }
                    }
                ],
                return_type: Some(parsing::Type::Identifier {
                    pos: Position::new(), identifier: "Num".to_string()
                }),
                body: vec![
                    parsing::Statement::Return(Some(parsing::Expression::Variable {
                        pos: Position::new(), identifier: "x".to_string()
                    }))
                ]
            }),
            Ok(None)
        );

        Ok(())
    }

    #[test]
    fn variable_shadowing() -> checking::Result<()> {
        let mut chkr = new_empty_checker();

        chkr.check_stmt(&parsing::Statement::VariableDeclaration {
            identifier: "x".to_string(),
            var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Num".to_string() },
            value: None
        })?;

        chkr.begin_new_scope();

        // Shadow variable 'x' by declaring a variable in the inner scope of the
        // same name but a different type:
        chkr.check_stmt(&parsing::Statement::VariableDeclaration {
            identifier: "x".to_string(),
            var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Bool".to_string() },
            value: None
        })?;

        assert_eq!(chkr.variable_lookup("x")?.var_type, checking::Type::Bool);

        chkr.end_scope();

        assert_eq!(chkr.variable_lookup("x")?.var_type, checking::Type::Num);

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "x".to_string(),
                var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Char".to_string() },
                value: None
            }),
            Err(checking::Failure::RedeclaredToDifferentType {
                identifier: "x".to_string(),
                expected: checking::Type::Num,
                encountered: checking::Type::Char
            })
        );

        Ok(())
    }

    #[test]
    fn empty_array_literals() {
        let mut chkr = new_empty_checker();

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "ok".to_string(),
                var_type: parsing::Type::Array(Box::new(parsing::Type::Identifier {
                    pos: Position::new(), identifier: "Num".to_string()
                })),
                value: Some(parsing::Expression::Array(vec![]))
            }),
            Ok(None)
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "bad".to_string(),
                var_type: parsing::Type::Identifier { pos: Position::new(), identifier: "Bool".to_string() },
                value: Some(parsing::Expression::Array(vec![]))
            }),
            Err(checking::Failure::UnexpectedType {
                expected: checking::Type::Bool,
                encountered: checking::Type::Array(Box::new(checking::Type::Any))
            })
        );

        assert_eq!(
            chkr.check_stmt(&parsing::Statement::VariableDeclaration {
                identifier: "wow".to_string(),
                var_type: parsing::Type::Array(Box::new(
                    parsing::Type::Array(Box::new(
                        parsing::Type::Identifier { pos: Position::new(), identifier: "Num".to_string() }
                    ))
                )),
                value: Some(parsing::Expression::Array(vec![
                    parsing::Expression::Array(vec![]),
                    parsing::Expression::Array(vec![
                        parsing::Expression::NumberLiteral { pos: Position::new(), value: 1.5 }
                    ])
                ]))
            }),
            Ok(None)
        );
    }
}