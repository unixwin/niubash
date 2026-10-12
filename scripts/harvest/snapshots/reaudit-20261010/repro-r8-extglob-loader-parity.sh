shopt -s extglob
case plugin::disable in
  plugin::@(disable|enable|load)) echo MATCHED ;;
esac
