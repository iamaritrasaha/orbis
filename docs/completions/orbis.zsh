#compdef orbis

_orbis() {
    local -a commands
    commands=(
        'dashboard:Launch the interactive Orbis dashboard'
        'ui:Alias for dashboard'
        'sources:Show detected providers and capabilities'
        'search:Search available providers'
        'info:Show normalized package metadata'
        'explain:Explain a package in plain language'
        'doctor:Run safe diagnostics'
        'install:Plan and install one exact package'
        'remove:Plan and remove one exact package'
        'update:Refresh package catalogs'
        'updates:Show available updates'
        'upgrade:Plan and apply available updates'
        'clean:Plan conservative cleanup'
        'history:Read transaction history'
        'why:Explain why a package is present'
    )
    _arguments \
        '1:command:->command' \
        '*::options:->options'

    case $state in
        command)
            _describe 'command' commands
            ;;
        options)
            _arguments \
                '--json[Emit structured JSON]' \
                '--no-color[Disable ANSI styling]' \
                '--plain[Force plain presentation]' \
                '--help[Print help]' \
                '--version[Print version]'
            ;;
    esac
}

_orbis "$@"
