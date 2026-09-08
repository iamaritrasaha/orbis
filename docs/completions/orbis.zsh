#compdef orbis

_orbis() {
    local -a commands
    commands=(
        'dashboard:Launch the interactive Orbis dashboard'
        'ui:Alias for dashboard'
        'sources:Show detected providers and capabilities'
        'find:Find software across supported systems'
        'show:Learn what software does and where it came from'
        'refresh:Refresh software information'
        'search:Search available providers'
        'info:Show normalized package metadata'
        'explain:Explain a package in plain language'
        'doctor:Run safe diagnostics'
        'install:Plan and install one exact package'
        'remove:Plan and remove one exact package'
        'update:Check for available software updates'
        'updates:Show available updates'
        'upgrade:Plan and apply available updates'
        'clean:Plan conservative cleanup'
        'history:Read transaction history'
        'why:Explain why a package is present'
        'health:Check that everything is working'
    )
    _arguments \
        '1:command:->command' \
        '*::options:->options'

    case $state in
        command)
            _describe 'command' commands
            ;;
        options)
            case $words[2] in
                update)
                    _arguments \
                        '--source[Restrict the check to one source]:source:(apt flatpak snap cargo npm pnpm uv pipx)' \
                        '--plan[Show the reviewed update plan]' \
                        '--apply[Review and apply updates]' \
                        '--yes[Skip the second confirmation]' \
                        '--json[Emit structured JSON]' \
                        '--no-color[Disable ANSI styling]' \
                        '--plain[Force plain presentation]' \
                        '--help[Print help]'
                    ;;
                refresh|upgrade|clean)
                    _arguments \
                        '--source[Restrict the operation to one source]:source:(apt flatpak snap cargo npm pnpm uv pipx)' \
                        '--plan[Show the plan without executing]' \
                        '--yes[Skip the second confirmation]' \
                        '--json[Emit structured JSON]' \
                        '--no-color[Disable ANSI styling]' \
                        '--plain[Force plain presentation]' \
                        '--help[Print help]'
                    ;;
                *)
                    _arguments \
                        '--json[Emit structured JSON]' \
                        '--no-color[Disable ANSI styling]' \
                        '--plain[Force plain presentation]' \
                        '--help[Print help]' \
                        '--version[Print version]'
                    ;;
            esac
            ;;
    esac
}

_orbis "$@"
