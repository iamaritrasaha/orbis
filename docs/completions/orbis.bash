# bash completion for Orbis 0.1.0-beta.1.dev.1

_orbis() {
    local cur prev words cword
    _init_completion || return

    local commands="dashboard ui sources search info explain doctor install remove update updates upgrade clean history why help"
    local global_options="--json --no-color --plain --help --version"
    if (( cword == 1 )); then
        COMPREPLY=( $(compgen -W "${commands} ${global_options}" -- "${cur}") )
        return
    fi

    case "${words[1]}" in
        search)
            COMPREPLY=( $(compgen -W "--source --json --no-color --plain --help" -- "${cur}") )
            ;;
        info|explain|why)
            COMPREPLY=( $(compgen -W "--source --json --no-color --plain --help" -- "${cur}") )
            ;;
        install|remove)
            COMPREPLY=( $(compgen -W "--source --scope --plan --dry-run --yes --json --no-color --plain --help" -- "${cur}") )
            ;;
        update|upgrade|clean)
            COMPREPLY=( $(compgen -W "--source --plan --yes --json --no-color --plain --help" -- "${cur}") )
            ;;
        updates|sources|doctor|dashboard|ui)
            COMPREPLY=( $(compgen -W "--json --no-color --plain --help" -- "${cur}") )
            ;;
        history)
            COMPREPLY=( $(compgen -W "--limit --source --json --no-color --plain --help" -- "${cur}") )
            ;;
        *)
            COMPREPLY=( $(compgen -W "${global_options}" -- "${cur}") )
            ;;
    esac
}

complete -F _orbis orbis
