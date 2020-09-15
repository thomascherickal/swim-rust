use crate::args::{ActionAttrs, AgentAttrs, CommandAttrs, MapAttrs, ValueAttrs};
use darling::FromMeta;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, DeriveInput};
mod args;

fn get_lifecycle_task_ident(name: &str) -> Ident {
    Ident::new(&format!("{}Task", name), Span::call_site())
}

#[proc_macro_attribute]
pub fn agent_lifecycle(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_ast = parse_macro_input!(input as DeriveInput);
    let attr_args = parse_macro_input!(args as AttributeArgs);

    let args = match AgentAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = &input_ast.ident;
    let task_name = get_lifecycle_task_ident(&input_ast.ident.to_string());
    let agent_name = &args.agent;
    let on_start_func = &args.on_start;

    let output_ast = quote! {

        #input_ast

        struct #task_name {
            lifecycle: #lifecycle_name
        }

        impl swim_server::agent::lifecycle::AgentLifecycle<#agent_name> for #task_name {
            fn on_start<'a, C>(&'a self, context: &'a C) -> futures::future::BoxFuture<'a, ()>
            where
                C: swim_server::agent::AgentContext<#agent_name> + Send + Sync + 'a,
            {
                #on_start_func(&self.lifecycle, context).boxed()
            }
        }

    };

    TokenStream::from(output_ast)
}

#[proc_macro_attribute]
pub fn command_lifecycle(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_ast = parse_macro_input!(input as DeriveInput);
    let attr_args = parse_macro_input!(args as AttributeArgs);

    let args = match CommandAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = &input_ast.ident;
    let task_name = get_lifecycle_task_ident(&input_ast.ident.to_string());
    let agent_name = &args.agent;
    let command_type = &args.command_type;
    let on_command_func = &args.on_command;

    let output_ast = quote! {

        #input_ast

        struct #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::CommandLane<#command_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, ()>> + Send + Sync + 'static
        {
            lifecycle: #lifecycle_name,
            name: String,
            event_stream: S,
            projection: T,
        }


        impl<T, S> swim_server::agent::Lane for #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::CommandLane<#command_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, ()>> + Send + Sync + 'static
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T, S> swim_server::agent::LaneTasks<#agent_name, Context> for #task_name<T, S>
        where
            Context: swim_server::agent::AgentContext<#agent_name> + swim_server::agent::context::AgentExecutionContext + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::CommandLane<#command_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, ()>> + Send + Sync + 'static
            {
                fn start<'a>(&'a self, _context: &'a Context) -> futures::future::BoxFuture<'a, ()> {
                    futures::future::ready(()).boxed()
                }

                fn events(self: Box<Self>, context: Context) -> futures::future::BoxFuture<'static, ()> {
                    async move {
                        let #task_name {
                            lifecycle,
                            event_stream,
                            projection,
                            ..
                        } = *self;

                        let model = projection(context.agent()).clone();
                        let mut events = event_stream.take_until(context.agent_stop_event());
                        pin_utils::pin_mut!(events);
                        while let Some(swim_server::agent::lane::model::action::Action { command, responder }) = events.next().await {

                        // Todo Failing to compile with swim_server::agent::COMMANDED
                            tracing::event!(tracing::Level::TRACE, COMMANDED, ?command);

                            tracing_futures::Instrument::instrument(
                                #on_command_func(&lifecycle, command, &model, &context),
                                tracing::span!(tracing::Level::TRACE, swim_server::agent::ON_COMMAND)
                            ).await;

                            if let Some(tx) = responder {
                                if tx.send(()).is_err() {
                                    // Todo Failing to compile with swim_server::agent::RESPONSE_IGNORED
                                    tracing::event!(tracing::Level::WARN, RESPONSE_IGNORED);
                                }
                            }
                        }
                    }
                    .boxed()
                }
            }

    };

    TokenStream::from(output_ast)
}

#[proc_macro_attribute]
pub fn action_lifecycle(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_ast = parse_macro_input!(input as DeriveInput);
    let attr_args = parse_macro_input!(args as AttributeArgs);

    let args = match ActionAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = &input_ast.ident;
    let task_name = get_lifecycle_task_ident(&input_ast.ident.to_string());
    let agent_name = &args.agent;
    let command_type = &args.command_type;
    let response_type = &args.response_type;
    let on_command_func = &args.on_command;

    let output_ast = quote! {

        #input_ast

        struct #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::ActionLane<#command_type, #response_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, #response_type>> + Send + Sync + 'static
        {
            lifecycle: #lifecycle_name,
            name: String,
            event_stream: S,
            projection: T,
        }

        impl<T, S> swim_server::agent::Lane for #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::ActionLane<#command_type, #response_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, #response_type>> + Send + Sync + 'static
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T, S> swim_server::agent::LaneTasks<#agent_name, Context> for #task_name<T, S>
        where
            Context: swim_server::agent::AgentContext<#agent_name> + swim_server::agent::context::AgentExecutionContext + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::action::ActionLane<#command_type, #response_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::action::Action<#command_type, #response_type>> + Send + Sync + 'static
        {
            fn start<'a>(&'a self, _context: &'a Context) -> futures::future::BoxFuture<'a, ()> {
                futures::future::ready(()).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> futures::future::BoxFuture<'static, ()> {
                async move {
                    let #task_name {
                        lifecycle,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until(context.agent_stop_event());
                    pin_utils::pin_mut!(events);
                    while let Some(swim_server::agent::lane::model::action::Action { command, responder }) = events.next().await {
                        tracing::event!(tracing::Level::TRACE, swim_server::agent::COMMANDED, ?command);

                        let response = tracing_futures::Instrument::instrument(
                                #on_command_func(&lifecycle, command, &model, &context),
                                tracing::span!(tracing::Level::TRACE, swim_server::agent::ON_COMMAND)
                            ).await;

                        tracing::event!(Level::TRACE, ACTION_RESULT, ?response);

                        if let Some(tx) = responder {
                            if tx.send(response).is_err() {
                                tracing::event!(tracing::Level::WARN, RESPONSE_IGNORED);
                            }
                        }
                    }
                }
                .boxed()
            }
        }

    };

    TokenStream::from(output_ast)
}

#[proc_macro_attribute]
pub fn value_lifecycle(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_ast = parse_macro_input!(input as DeriveInput);
    let attr_args = parse_macro_input!(args as AttributeArgs);

    let args = match ValueAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = &input_ast.ident;
    let task_name = get_lifecycle_task_ident(&input_ast.ident.to_string());
    let agent_name = &args.agent;
    let event_type = &args.event_type;
    let on_start_func = &args.on_start;
    let on_event_func = &args.on_event;

    let output_ast = quote! {

        #input_ast

        struct #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::value::ValueLane<#event_type> + Send + Sync + 'static,
            S: futures::Stream<Item = std::sync::Arc<#event_type>> + Send + Sync + 'static
        {
            lifecycle: #lifecycle_name,
            name: String,
            event_stream: S,
            projection: T,
        }

        impl<T, S> swim_server::agent::Lane for #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::value::ValueLane<#event_type> + Send + Sync + 'static,
            S: futures::Stream<Item = std::sync::Arc<#event_type>> + Send + Sync + 'static
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T, S> swim_server::agent::LaneTasks<#agent_name, Context> for #task_name<T, S>
        where
            Context: swim_server::agent::AgentContext<#agent_name> + swim_server::agent::context::AgentExecutionContext + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::value::ValueLane<#event_type> + Send + Sync + 'static,
            S: futures::Stream<Item = std::sync::Arc<#event_type>> + Send + Sync + 'static
        {
            fn start<'a>(&'a self, context: &'a Context) -> futures::future::BoxFuture<'a, ()> {
                let #task_name { lifecycle, projection, .. } = self;

                let model = projection(context.agent());
                #on_start_func(lifecycle, model, context).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> futures::future::BoxFuture<'static, ()> {
                async move {
                    let #task_name {
                        lifecycle,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until(context.agent_stop_event());
                    pin_utils::pin_mut!(events);
                    while let Some(event) = events.next().await {
                        tracing_futures::Instrument::instrument(
                                #on_event_func(&lifecycle, &event, &model, &context),
                                tracing::span!(tracing::Level::TRACE, swim_server::agent::ON_EVENT, ?event)
                        ).await;
                    }
                }
                .boxed()
            }
        }

    };

    TokenStream::from(output_ast)
}

#[proc_macro_attribute]
pub fn map_lifecycle(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_ast = parse_macro_input!(input as DeriveInput);
    let attr_args = parse_macro_input!(args as AttributeArgs);

    let args = match MapAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = &input_ast.ident;
    let task_name = get_lifecycle_task_ident(&input_ast.ident.to_string());
    let agent_name = &args.agent;
    let key_type = &args.key_type;
    let value_type = &args.value_type;
    let on_start_func = &args.on_start;
    let on_event_func = &args.on_event;

    let output_ast = quote! {

        #input_ast

        struct #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::map::MapLane<#key_type, #value_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::map::MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static
        {
            lifecycle: #lifecycle_name,
            name: String,
            event_stream: S,
            projection: T,
        }

        impl<T, S> swim_server::agent::Lane for #task_name<T, S>
        where
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::map::MapLane<#key_type, #value_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::map::MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T, S> swim_server::agent::LaneTasks<#agent_name, Context> for #task_name<T, S>
        where
            Context: swim_server::agent::AgentContext<#agent_name> + swim_server::agent::context::AgentExecutionContext + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &swim_server::agent::lane::model::map::MapLane<#key_type, #value_type> + Send + Sync + 'static,
            S: futures::Stream<Item = swim_server::agent::lane::model::map::MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static
        {
            fn start<'a>(&'a self, context: &'a Context) -> futures::future::BoxFuture<'a, ()> {
                let #task_name { lifecycle, projection, .. } = self;

                let model = projection(context.agent());
                #on_start_func(lifecycle, model, context).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> futures::future::BoxFuture<'static, ()> {
                async move {
                    let #task_name {
                        lifecycle,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until(context.agent_stop_event());
                    pin_utils::pin_mut!(events);
                    while let Some(event) = events.next().await {
                        tracing_futures::Instrument::instrument(
                                #on_event_func(&lifecycle, &event, &model, &context),
                                tracing::span!(tracing::Level::TRACE, swim_server::agent::ON_EVENT, ?event)
                        ).await;
                    }
                }
                .boxed()
            }
        }

    };

    TokenStream::from(output_ast)
}
