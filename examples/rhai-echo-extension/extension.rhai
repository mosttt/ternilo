// Every contributed handler receives a safe context, JSON arguments, and mount settings.
fn echo_handler(context, arguments, settings) {
    #{
        content: settings.prefix + arguments.text,
        is_error: false
    }
}

// Returning any other map produces JSON tool output.
fn inspect_handler(context, arguments, settings) {
    #{
        label: settings.label,
        arguments: arguments,
        session_id: context.session_id
    }
}
