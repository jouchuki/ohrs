#!/usr/bin/env bash
# Bash completion for ohrs (OpenHarness RS)

_ohrs_completions() {
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    # All flags
    opts="-c --continue -r --resume -n --name -m --model --effort --max-turns -p --print --output-format --permission-mode --dangerously-skip-permissions -s --system-prompt --append-system-prompt --settings -d --debug --mcp-config --bare -h --help -V --version"

    # Value completions for specific flags
    case "${prev}" in
        -m|--model)
            COMPREPLY=( $(compgen -W "claude-sonnet-4-6 claude-opus-4-6 claude-haiku-4-5" -- "${cur}") )
            return 0
            ;;
        --effort)
            COMPREPLY=( $(compgen -W "low medium high" -- "${cur}") )
            return 0
            ;;
        --permission-mode)
            COMPREPLY=( $(compgen -W "default plan full_auto" -- "${cur}") )
            return 0
            ;;
        --output-format)
            COMPREPLY=( $(compgen -W "text json" -- "${cur}") )
            return 0
            ;;
        --settings|--mcp-config)
            COMPREPLY=( $(compgen -f -- "${cur}") )
            return 0
            ;;
        -r|--resume|-n|--name)
            # Could complete session names/IDs — for now no completion
            return 0
            ;;
        -p|--print|-s|--system-prompt|--append-system-prompt)
            # Free text — no completion
            return 0
            ;;
        --max-turns)
            COMPREPLY=( $(compgen -W "1 4 8 16 32" -- "${cur}") )
            return 0
            ;;
    esac

    # Flag completion
    if [[ "${cur}" == -* ]]; then
        COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
        return 0
    fi
}

complete -F _ohrs_completions ohrs
complete -F _ohrs_completions oh
