f() {
if [[
  ( -z "$ExactMatch" && "$ArityMin" = "$ArityMax" )
  || ((${#_ARGS_BOUNDS[@]} == 0))
]]; then
  :
fi
}
