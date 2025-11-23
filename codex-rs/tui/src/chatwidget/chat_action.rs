
#[derive(Debug, PartialEq)]
enum ChatAction {
    List,
    Delete { id: String },
    Resume { id: String },
    Share { id: String },
}

fn parse_chat_args(args: &str) -> Result<ChatAction, String> {
    let args: Vec<&str> = args.split_whitespace().collect();
    if args.is_empty() {
        return Ok(ChatAction::List);
    }
    match args[0] {
        "list" | "ls" => Ok(ChatAction::List),
        "delete" | "rm" => {
            if args.len() < 2 {
                return Err("Usage: /chat delete <id>".to_string());
            }
            Ok(ChatAction::Delete { id: args[1].to_string() })
        }
        "resume" => {
            if args.len() < 2 {
                return Err("Usage: /chat resume <id>".to_string());
            }
            Ok(ChatAction::Resume { id: args[1].to_string() })
        }
        "share" => {
            if args.len() < 2 {
                return Err("Usage: /chat share <id>".to_string());
            }
            Ok(ChatAction::Share { id: args[1].to_string() })
        }
        _ => Err(format!("Unknown subcommand: {}", args[0])),
    }
}
