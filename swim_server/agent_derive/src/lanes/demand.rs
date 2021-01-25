// Copyright 2015-2020 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::lanes::derive_lane;
use crate::utils::{
    get_task_struct_name, parse_callback, validate_input_ast, Callback, CallbackKind, InputAstType,
    LaneTasksImpl,
};
use darling::FromMeta;
use macro_helpers::{has_fields, string_to_ident};
use proc_macro::TokenStream;
use quote::quote;
use syn::{AttributeArgs, DeriveInput, Ident};

#[derive(Debug, FromMeta)]
struct DemandAttrs {
    #[darling(map = "string_to_ident")]
    agent: Ident,
    #[darling(map = "string_to_ident")]
    event_type: Ident,
    #[darling(default)]
    on_cue: Option<darling::Result<String>>,
}

pub fn derive_demand_lifecycle(attr_args: AttributeArgs, input_ast: DeriveInput) -> TokenStream {
    if let Err(error) = validate_input_ast(&input_ast, InputAstType::Lifecycle) {
        return TokenStream::from(quote! {#error});
    }

    let args = match DemandAttrs::from_list(&attr_args) {
        Ok(args) => args,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let lifecycle_name = input_ast.ident.clone();
    let has_fields = has_fields(&input_ast.data);
    let task_name = get_task_struct_name(&input_ast.ident.to_string());
    let agent_name = args.agent.clone();
    let event_type = &args.event_type;
    let on_cue_callback = parse_callback(&args.on_cue, task_name.clone(), CallbackKind::Cue);
    let lane_tasks_impl = LaneTasksImpl::Demand {
        on_cue: on_cue_callback,
    };

    let extra_field = Some(quote! {
        response_tx: tokio::sync::mpsc::Sender<#event_type>
    });

    derive_lane(
        "DemandLifecycle",
        lifecycle_name,
        has_fields,
        task_name,
        agent_name,
        input_ast,
        quote!(swim_server::agent::lane::model::demand::DemandLane<#event_type>),
        quote!(()),
        lane_tasks_impl,
        quote! {
            use swim_server::agent::lane::model::demand::DemandLane;
            use swim_server::agent::lane::lifecycle::LaneLifecycle;
        },
        extra_field,
    )
}

pub fn derive_events_body(on_cue: &Callback) -> proc_macro2::TokenStream {
    let task_name = &on_cue.task_name;
    let on_cue_func_name = &on_cue.func_name;

    quote!(
        let #task_name {
            lifecycle,
            event_stream,
            projection,
            response_tx,
            ..
        } = *self;

        let model = projection(context.agent()).clone();
        let mut events = event_stream.take_until(context.agent_stop_event());
        let mut events = unsafe { Pin::new_unchecked(&mut events) };

        while let Some(event) = events.next().await {
            if let Some(value) = lifecycle.#on_cue_func_name(&model, &context).await {
                let _ = response_tx.send(value).await;
            }
        }
    )
}
