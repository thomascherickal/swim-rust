use crate::args::{ActionAttrs, AgentAttrs, CommandAttrs, MapAttrs, ValueAttrs};
use darling::FromMeta;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, DeriveInput};
mod args;

fn get_lifecycle_ident(name: &str) -> Ident {
    Ident::new(&format!("{}Lifecycle", name), Span::call_site())
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

    let inner_struct_name = &input_ast.ident;
    let lifecycle_name = get_lifecycle_ident(&input_ast.ident.to_string());
    let agent_name = Ident::new(&args.agent, Span::call_site());
    let on_start_func = Ident::new(&args.on_start, Span::call_site());

    let output_ast = quote! {

        #input_ast

        struct #lifecycle_name {
            inner: #inner_struct_name
        }

        impl AgentLifecycle<#agent_name> for #lifecycle_name {
            fn on_start<'a, C>(&'a self, context: &'a C) -> BoxFuture<'a, ()>
            where
                C: AgentContext<#agent_name> + Send + Sync + 'a,
            {
                #on_start_func(&self.inner, context).boxed()
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

    let inner_struct_name = &input_ast.ident;
    let lifecycle_name = get_lifecycle_ident(&input_ast.ident.to_string());
    let agent_name = Ident::new(&args.agent, Span::call_site());
    let command_type = Ident::new(&args.command_type, Span::call_site());
    let on_command_func = Ident::new(&args.on_command, Span::call_site());

    let output_ast = quote! {

        #input_ast

        struct #lifecycle_name<T>
        where
            T: Fn(&#agent_name) -> &CommandLane<#command_type> + Send + Sync + 'static,
        {
            inner: #inner_struct_name,
            name: String,
            event_stream: mpsc::Receiver<#command_type>,
            projection: T,
        }


        impl<T: Fn(&#agent_name) -> &CommandLane<#command_type> + Send + Sync + 'static> Lane
        for #lifecycle_name<T>
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T> LaneTasks<#agent_name, Context> for #lifecycle_name<T>
        where
            Context: AgentContext<#agent_name> + Sized + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &CommandLane<#command_type> + Send + Sync + 'static,
            {
                fn start<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
                    ready(()).boxed()
                }

                fn events(self: Box<Self>, context: Context) -> BoxFuture<'static, ()> {
                    async move {
                        let #lifecycle_name {
                            inner,
                            event_stream,
                            projection,
                            ..
                        } = *self;

                        let model = projection(context.agent()).clone();
                        let mut events = event_stream.take_until_completes(context.agent_stop_event());
                        pin_mut!(events);
                        while let Some(command) = events.next().await {
                            event!(Level::TRACE, COMMANDED, ?command);
                            #on_command_func(&inner, command, &model, &context)
                                .instrument(span!(Level::TRACE, ON_COMMAND))
                                .await;
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

    let inner_struct_name = &input_ast.ident;
    let lifecycle_name = get_lifecycle_ident(&input_ast.ident.to_string());
    let agent_name = Ident::new(&args.agent, Span::call_site());
    let command_type = Ident::new(&args.command_type, Span::call_site());
    let response_type = Ident::new(&args.response_type, Span::call_site());
    let on_command_func = Ident::new(&args.on_command, Span::call_site());

    let output_ast = quote! {

        #input_ast

        struct #lifecycle_name<T>
        where
            T: Fn(&#agent_name) -> &ActionLane<#command_type, #response_type> + Send + Sync + 'static,
        {
            inner: #inner_struct_name,
            name: String,
            event_stream: mpsc::Receiver<#command_type>,
            projection: T,
        }

        impl<T: Fn(&#agent_name) -> &ActionLane<#command_type, #response_type> + Send + Sync + 'static> Lane
        for #lifecycle_name<T>
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T> LaneTasks<#agent_name, Context> for #lifecycle_name<T>
        where
            Context: AgentContext<#agent_name> + Sized + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &ActionLane<#command_type, #response_type> + Send + Sync + 'static,
        {
            fn start<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
                ready(()).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> BoxFuture<'static, ()> {
                async move {
                    let #lifecycle_name {
                        inner,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until_completes(context.agent_stop_event());
                    pin_mut!(events);
                    while let Some(command) = events.next().await {
                        event!(Level::TRACE, COMMANDED, ?command);
                        let response = #on_command_func(&inner, command, &model, &context)
                            .instrument(span!(Level::TRACE, ON_COMMAND))
                            .await;
                        event!(Level::TRACE, ACTION_RESULT, ?response);
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

    let inner_struct_name = &input_ast.ident;
    let lifecycle_name = get_lifecycle_ident(&input_ast.ident.to_string());
    let agent_name = Ident::new(&args.agent, Span::call_site());
    let event_type = Ident::new(&args.event_type, Span::call_site());
    let on_start_func = Ident::new(&args.on_start, Span::call_site());
    let on_event_func = Ident::new(&args.on_event, Span::call_site());

    let output_ast = quote! {

        #input_ast

        struct #lifecycle_name<T>
        where
            T: Fn(&#agent_name) -> &ValueLane<#event_type> + Send + Sync + 'static,
        {
            inner: #inner_struct_name,
            name: String,
            event_stream: mpsc::Receiver<Arc<#event_type>>,
            projection: T,
        }

        impl<T: Fn(&#agent_name) -> &ValueLane<#event_type> + Send + Sync + 'static> Lane for #lifecycle_name<T> {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T> LaneTasks<#agent_name, Context> for #lifecycle_name<T>
        where
            Context: AgentContext<#agent_name> + Sized + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &ValueLane<i32> + Send + Sync + 'static,
        {
            fn start<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
                let #lifecycle_name { inner, projection, .. } = self;

                let model = projection(context.agent());
                #on_start_func(inner, model, context).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> BoxFuture<'static, ()> {
                async move {
                    let #lifecycle_name {
                        inner,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until_completes(context.agent_stop_event());
                    pin_mut!(events);
                    while let Some(event) = events.next().await {
                        #on_event_func(&inner, &event, &model, &context)
                            .instrument(span!(Level::TRACE, ON_EVENT, ?event))
                            .await;
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

    let inner_struct_name = &input_ast.ident;
    let lifecycle_name = get_lifecycle_ident(&input_ast.ident.to_string());
    let agent_name = Ident::new(&args.agent, Span::call_site());
    let key_type = Ident::new(&args.key_type, Span::call_site());
    let value_type = Ident::new(&args.value_type, Span::call_site());
    let on_start_func = Ident::new(&args.on_start, Span::call_site());
    let on_event_func = Ident::new(&args.on_event, Span::call_site());

    let output_ast = quote! {

        #input_ast

        struct #lifecycle_name<T, S>
        where
            T: Fn(&#agent_name) -> &MapLane<#key_type, #value_type> + Send + Sync + 'static,
            S: Stream<Item = MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static,
        {
            inner: #inner_struct_name,
            name: String,
            event_stream: S,
            projection: T,
        }

        impl<T, S> Lane for #lifecycle_name<T, S>
        where
            T: Fn(&#agent_name) -> &MapLane<#key_type, #value_type> + Send + Sync + 'static,
            S: Stream<Item = MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static,
        {
            fn name(&self) -> &str {
                &self.name
            }
        }

        impl<Context, T, S> LaneTasks<#agent_name, Context> for #lifecycle_name<T, S>
        where
            Context: AgentContext<#agent_name> + Sized + Send + Sync + 'static,
            S: Stream<Item = MapLaneEvent<#key_type, #value_type>> + Send + Sync + 'static,
            T: Fn(&#agent_name) -> &MapLane<#key_type, #value_type> + Send + Sync + 'static,
        {
            fn start<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
                let #lifecycle_name { inner, projection, .. } = self;

                let model = projection(context.agent());
                #on_start_func(inner, model, context).boxed()
            }

            fn events(self: Box<Self>, context: Context) -> BoxFuture<'static, ()> {
                async move {
                    let #lifecycle_name {
                        inner,
                        event_stream,
                        projection,
                        ..
                    } = *self;

                    let model = projection(context.agent()).clone();
                    let mut events = event_stream.take_until_completes(context.agent_stop_event());
                    pin_mut!(events);
                    while let Some(event) = events.next().await {
                        #on_event_func(&inner, &event, &model, &context)
                            .instrument(span!(Level::TRACE, ON_EVENT, ?event))
                            .await;
                    }
                }
                .boxed()
            }
        }

    };

    TokenStream::from(output_ast)
}
