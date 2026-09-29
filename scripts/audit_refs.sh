#!/usr/bin/env bash
#
# Every citation in `docs/audit.md`, checked against the tree it points at.
#
#   scripts/audit_refs.sh          print the citations that no longer hold
#   scripts/audit_refs.sh --all    print every citation that was read, and what it resolved to
#
# The audit is an argument made out of evidence, and its evidence is mostly a
# `path:line` or a test's name. Both rot in the ordinary course of work: a lane
# inserts a function above what a row cites, a test is renamed, a file is split.
# This script reads the document the way a reader does — every backticked span is
# a citation — and asks the tree whether each one still holds:
#
#   * a file must be there: a path as written (`crates/…`, `docs/…`), or a bare
#     name resolved by searching the crates, since rows write both `api.rs:732`
#     and `stream.rs:221-255`;
#   * a line, or a list like `:31,275-307,506`, must be inside the file; and when
#     the citation promises a name — `` `…/empty_tab.rs:612` (`render_choosers`) ``
#     — that line has to hold it. This is the check that catches the rot the
#     others cannot: a lane inserts a function above a citation, the file grows,
#     every path and every test name still resolves, and the row now points at the
#     wrong place. Only a name is checked; a parenthesis of prose (`(24 h
#     staleness)`, `(block, markers, path)`) says nothing a script can hold it to;
#   * a cited test must still be a `fn` in the file it names. Five spellings are
#     in the document: `crate::tests/file.rs::name`, `crate::file.rs::name`,
#     `module::tests::name`, `file.rs::name`, and a bare `::name` continuing the
#     file named earlier in the same cell, row or section;
#   * a directory a row names (`crates/settings`) must exist.
#
# A bare `:450` is the interesting case: the row it sits in names no file, because
# §5's table header says "evidence (all in `crates/session/src/model.rs` unless
# noted)". So a citation is attributed to the last file named before it — in its
# cell, else in its row, else in the section ("all in `…`"). A citation whose file
# cannot be attributed at all is printed as *unattributed* rather than passed
# silently: a script that shrugs is how a stale citation survives the check
# written to find it.
#
# Not every backticked span is a citation: `--no-userspace`, `/state`, `std::net`,
# `app.json`, `<folder>/.evo/swarm.lisp`, prose. Those are skipped — a span that
# names no file we can find and no function is not evidence we can check.
#
# Run it after the lanes stop writing: a file another lane is editing *right now*
# makes the count move between two runs, and a line is only wrong relative to a
# tree that is holding still.
#
# Nothing is rewritten and nothing is built: `find`, `grep` and `wc` over the
# checkout. Exit status is 1 when a citation is broken, so this can sit in front of
# a refresh that is meant to leave nothing stale behind.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")

show_all=0
audit="$root/docs/audit.md"
for arg in "$@"; do
  case "$arg" in
    --all) show_all=1 ;;
    -*) echo "usage: $(basename "$0") [--all] [audit.md]" >&2; exit 2 ;;
    *) audit="$arg" ;;
  esac
done
[ -f "$audit" ] || { echo "no such audit: $audit" >&2; exit 2; }
doc=$(basename "$audit")

checked=0
broken=0
unattributed=0

# Every file this script could ever resolve to, once: the alternative is a `find`
# per citation, and the audit holds hundreds.
file_index=$(mktemp -t audit-refs.XXXXXX)
manifest_map=$(mktemp -t audit-refs-manifests.XXXXXX)
trap 'rm -f "$file_index" "$manifest_map"' EXIT
for manifest in "$root"/crates/*/Cargo.toml; do
  package=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)
  [ -n "$package" ] && printf '%s %s\n' "$(printf '%s' "$package" | tr '-' '_')" "$(dirname "$manifest")" >>"$manifest_map"
done
find "$root/crates" "$root/docs" "$root/scripts" -type f 2>/dev/null >"$file_index" || true
for extra in "$root/README.md" "$root/Cargo.toml"; do
  [ -f "$extra" ] && printf '%s\n' "$extra" >>"$file_index"
done
sort -o "$file_index" "$file_index"

# Candidates by the name they end in: a full path (`docs/proofs.md`) and a bare
# name (`stream.rs`) are the same question with a different suffix.
by_suffix() { # $1 = what the path must end in
  # Literally, with awk's `index()` rather than a regex: these names carry dots and
  # dashes, and hand-escaping them for `grep -E` is how a checker starts lying.
  awk -v suffix="/$1" 'index($0, suffix) == length($0) - length(suffix) + 1' "$file_index" 2>/dev/null
}

# A row cites a crate by package name (`evo_desktop::tests/quit.rs`), and a package
# is not always a directory (`evo_desktop` is `crates/app`). The manifests are read
# into a two-column file once — bash 3.2 has no associative arrays — and the
# directory a citation names is looked up there.
crate_dir_of() { # $1 = package name as written; prints its crate directory
  local want hit
  want=$(printf '%s' "$1" | tr '-' '_')
  hit=$(awk -v want="$want" '$1 == want { print $2; exit }' "$manifest_map")
  if [ -n "$hit" ]; then
    printf '%s\n' "$hit"
    return 0
  fi
  if [ -d "$root/crates/$1" ]; then
    printf '%s\n' "$root/crates/$1"
    return 0
  fi
  return 1
}

# Where a cited file may be: as written, or — for a name a row leaves to the
# reader — any file carrying it, in the crate the prefix names or anywhere.
candidates_for() { # $1 = prefix (crate or module, may be empty), $2 = file or module part
  local prefix=$1 part=$2 dir=""
  [ -n "$prefix" ] && dir=$(crate_dir_of "$prefix" || true)
  if [ -z "$part" ]; then
    # `crate::tests` — whatever the prefix's own sources and tests are.
    [ -n "$dir" ] && grep -E "^$dir/(src|tests)/.*\.rs$" "$file_index" 2>/dev/null
    return 0
  fi
  # A path written out: `crates/x/src/y.rs`, `docs/proofs.md`, `Cargo.toml`.
  if [[ $part == */* ]]; then
    [ -f "$root/$part" ] && printf '%s\n' "$root/$part"
    [ -n "$dir" ] && printf '%s\n' "$dir/$part"
    by_suffix "$part"
    return 0
  fi
  if [[ $part == *.rs ]]; then
    [ -f "$root/$part" ] && printf '%s\n' "$root/$part"
    [ -n "$dir" ] && printf '%s\n' "$dir/src/$part"
    [ -n "$dir" ] && printf '%s\n' "$dir/$part"
    by_suffix "$part"
    return 0
  fi
  if [[ $part == *.* ]]; then
    # A file with some other extension, named by its own name (`Cargo.toml`).
    [ -f "$root/$part" ] && printf '%s\n' "$root/$part"
    [ -n "$dir" ] && printf '%s\n' "$dir/$part"
    by_suffix "$part"
    return 0
  fi
  # A module or crate name: its file, a file named after it, or its crate's sources
  # (a module's tests can sit in `lib.rs`, as `composer::tests::…` does).
  [ -n "$dir" ] && printf '%s\n' "$dir/src/$part.rs"
  by_suffix "$part.rs"
  [ -n "$dir" ] && grep -E "^$dir/src/.*\.rs$" "$file_index" 2>/dev/null
  return 0
}

# What the paragraph of a citation promises about the line it names: rows write
# `` `crates/workspace/src/empty_tab.rs:612` (`render_choosers`) ``, and that
# parenthetical is checkable — the line has to hold the name. Only a name is
# checkable; a parenthesis of prose (`(24 h staleness)`, `(block, markers, path)`)
# is skipped.
promised_name() { # $1 = the next span in the cell, if it is one
  local name_re='^[A-Za-z_][A-Za-z0-9_:]*$'
  case "$1" in
    *' '*|*','*|*'('*|*')'*|*'.'*) return 1 ;;
  esac
  [[ $1 =~ $name_re ]] || return 1
  # `…::tests::name` is the next citation, not a promise about this line.
  case "$1" in *'::tests::'*|*'::tests'*) return 1 ;; esac
  # `store::swarm_config` is a module a row names next, and `swarm_client` is a
  # crate: neither is a name the cited line has to hold.
  [[ $1 =~ ^[a-z0-9_]+::[a-z0-9_]+$ ]] && return 1
  # A symbol is snake_case, or `Type::method` — a bare word is prose too often.
  case "$1" in *_*) ;; *) return 1 ;; esac
  if crate_dir_of "$1" >/dev/null 2>&1; then
    return 1
  fi
  [ -n "$(by_suffix "$1.rs")" ] && return 1
  return 0
}

# What a promise is checked as: the name itself, without the type it hangs off —
# rows write `store::history::scan_in_background` while the line says
# `history::scan_in_background`.
short_name() { # $1 = a promised name
  printf '%s' "${1##*::}"
}

# The line a name sits on in $1, or empty when the file does not hold it at all.
name_line() { # $1 = file, $2 = name
  grep -nF -m1 "$2" "$1" 2>/dev/null | cut -d: -f1 || true
}

# The first candidate that exists, preferring one that carries the cited function.
pick_candidate() { # $1 = prefix, $2 = part, $3 = fn name (may be empty); prints the file
  local prefix=$1 part=$2 name=$3 candidate first=""
  while IFS= read -r candidate; do
    [ -f "$candidate" ] || continue
    if [ -n "$name" ] && grep -qE "fn ${name}\(" "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
    [ -z "$first" ] && first=$candidate
  done < <(candidates_for "$prefix" "$part")
  [ -n "$first" ] && printf '%s\n' "$first"
  return 0
}

report() { # $1 = audit line, $2 = citation, $3 = reason
  broken=$((broken + 1))
  printf 'BROKEN  %s:%s  %s\n          %s\n' "$doc" "$1" "$2" "$3"
}

unattributed_ref() { # $1 = audit line, $2 = citation, $3 = why it could not be attributed
  unattributed=$((unattributed + 1))
  printf 'UNATTRIBUTED  %s:%s  %s\n              %s\n' "$doc" "$1" "$2" "$3"
}

note() { # --all: what was read, and what it resolved to
  [ "$show_all" = 1 ] && printf 'ok  %s:%s  %s  ->  %s\n' "$doc" "$1" "$2" "$3"
  return 0
}

# `12,15-17,40` -> one number per line.
each_line() {
  local part
  tr ',' '\n' <<<"$1" | while IFS= read -r part; do
    [ -z "$part" ] && continue
    if [[ $part == *-* ]]; then
      seq "${part%%-*}" "${part##*-}"
    else
      printf '%s\n' "$part"
    fi
  done
}

check_lines() { # $1 = file, $2 = line spec; prints why (or nothing when they are inside)
  local file=$1 len n
  len=$(wc -l <"$file" | tr -d ' ')
  while IFS= read -r n; do
    if [ "$n" -lt 1 ] || [ "$n" -gt "$len" ]; then
      printf 'line %s is past the end of %s (%s lines)' "$n" "${file#"$root"/}" "$len"
      return 0
    fi
  done < <(each_line "$2")
  return 0
}

# A span that names no file and no function is not something this script can check:
# a flag, an HTTP path, a runtime file in `~/.evo/desktop`, an illustrative path.
checkable() { # $1 = token
  case "$1" in
    *'<'*|*'>'*|*' '*|*'*'*) return 1 ;;
  esac
  case "$1" in
    *".rs"*|*".toml"*|*".md"*|*".sh"*) return 0 ;;
  esac
  [[ $1 =~ ^:[0-9] ]] && return 0
  # `::name` and `crate::name` both cite something, even when the file they belong
  # to is only implied.
  [[ $1 =~ ^::[a-z0-9_]+$ ]] && return 0
  [[ $1 =~ ^[a-z0-9_]+:: ]] && return 0
  [[ $1 =~ ^(crates|docs|scripts)/ ]] && return 0
  return 1
}

cell_file=""
row_file=""
scope_file=""
in_fence=0
line_no=0
while IFS= read -r line; do
  line_no=$((line_no + 1))
  case "$line" in
    '```'*) in_fence=$((1 - in_fence)); continue ;;
  esac
  [ "$in_fence" = 1 ] && continue
  # A section states its own evidence file when its rows do not: "§5 Events —
  # evidence (all in `crates/session/src/model.rs` unless noted)".
  if [[ $line == '#'* ]]; then
    scope_file=""
  elif [[ $line =~ all\ in\ \`([A-Za-z0-9_./-]+\.rs)\` ]]; then
    scope_file=$(pick_candidate "" "${BASH_REMATCH[1]}" "")
  fi
  row_file=""
  while IFS= read -r cell; do
    cell_file=""
    spans=()
    while IFS= read -r span; do
      spans+=("$span")
    done < <(grep -oE '`[^`]+`' <<<"$cell" || true)
    span_count=${#spans[@]}
    index=0
    while [ "$index" -lt "$span_count" ]; do
      span=${spans[$index]}
      index=$((index + 1))
      token=${span#\`}
      token=${token%\`}
      [ -z "$token" ] && continue
      checkable "$token" || continue

      # `::test_name` — the file this cell, this row or this section named.
      if [[ $token =~ ^::([a-z0-9_]+)$ ]]; then
        name=${BASH_REMATCH[1]}
        file=$cell_file
        [ -z "$file" ] && file=$row_file
        [ -z "$file" ] && file=$scope_file
        checked=$((checked + 1))
        if [ -z "$file" ]; then
          unattributed_ref "$line_no" "$token" "a test name with no file named near it (cell, row, section)"
        elif grep -qE "fn ${name}\(" "$file"; then
          note "$line_no" "$token" "$name in ${file#"$root"/}"
        else
          report "$line_no" "$token" "no \`fn ${name}\` in ${file#"$root"/}"
        fi
        continue
      fi

      # `crate::tests/file.rs::test_name` and `crate::file.rs::test_name`.
      if [[ $token =~ ^([a-z0-9_]+)::([A-Za-z0-9_./-]+\.rs)::([a-z0-9_]+)$ ]]; then
        crate=${BASH_REMATCH[1]}
        part=${BASH_REMATCH[2]}
        name=${BASH_REMATCH[3]}
        checked=$((checked + 1))
        file=$(pick_candidate "$crate" "$part" "$name")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no $part in any crate (prefix \`$crate\`)"
          continue
        fi
        cell_file=$file
        row_file=$file
        if grep -qE "fn ${name}\(" "$file"; then
          note "$line_no" "$token" "$name in ${file#"$root"/}"
        else
          report "$line_no" "$token" "no \`fn ${name}\` in ${file#"$root"/}"
        fi
        continue
      fi

      # `crate::tests/file.rs` — the file, no single test named.
      if [[ $token =~ ^([a-z0-9_]+)::([A-Za-z0-9_./-]+\.rs)$ ]]; then
        crate=${BASH_REMATCH[1]}
        part=${BASH_REMATCH[2]}
        checked=$((checked + 1))
        file=$(pick_candidate "$crate" "$part" "")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no $part in any crate (prefix \`$crate\`)"
          continue
        fi
        cell_file=$file
        row_file=$file
        note "$line_no" "$token" "${file#"$root"/}"
        continue
      fi

      # `module::tests::test_name` — the module's file, or its crate's sources.
      if [[ $token =~ ^([a-z0-9_]+)::tests::([a-z0-9_]+)$ ]]; then
        stem=${BASH_REMATCH[1]}
        name=${BASH_REMATCH[2]}
        checked=$((checked + 1))
        # `empty_tab::tests::…` is a module in another crate's source tree, while
        # `composer::tests::…` is a whole crate: try the file named after the module
        # first, then the crate's own sources.
        file=$(pick_candidate "" "$stem" "$name")
        [ -z "$file" ] && file=$(pick_candidate "$stem" "tests" "$name")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no \`fn ${name}\` under $stem"
          continue
        fi
        cell_file=$file
        row_file=$file
        if grep -qE "fn ${name}\(" "$file"; then
          note "$line_no" "$token" "$name in ${file#"$root"/}"
        else
          report "$line_no" "$token" "no \`fn ${name}\` in ${file#"$root"/}"
        fi
        continue
      fi

      # `module::tests` — a module's test file, named but not cited down to a test.
      if [[ $token =~ ^([a-z0-9_]+)::tests$ ]]; then
        checked=$((checked + 1))
        file=$(pick_candidate "${BASH_REMATCH[1]}" "tests" "")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no test file under ${BASH_REMATCH[1]}"
          continue
        fi
        cell_file=$file
        row_file=$file
        note "$line_no" "$token" "${file#"$root"/}"
        continue
      fi

      # `file.rs::test_name`, with the file named by path or by its own name.
      if [[ $token =~ ^([A-Za-z0-9_./-]+\.rs)::([a-z0-9_]+)$ ]]; then
        part=${BASH_REMATCH[1]}
        name=${BASH_REMATCH[2]}
        checked=$((checked + 1))
        file=$(pick_candidate "" "$part" "$name")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no such file: $part"
          continue
        fi
        cell_file=$file
        row_file=$file
        if grep -qE "fn ${name}\(" "$file"; then
          note "$line_no" "$token" "$name in ${file#"$root"/}"
        else
          report "$line_no" "$token" "no \`fn ${name}\` in ${file#"$root"/}"
        fi
        continue
      fi

      # `crate::module::function` — a function cited without a line.
      if [[ $token =~ ^([a-z0-9_]+)::([a-z0-9_]+)::([a-z0-9_]+)$ ]]; then
        crate=${BASH_REMATCH[1]}
        module=${BASH_REMATCH[2]}
        name=${BASH_REMATCH[3]}
        dir=$(crate_dir_of "$crate" || true)
        checked=$((checked + 1))
        if [ -n "$dir" ] && grep -rqE "fn ${name}\(" "$dir" --include='*.rs'; then
          note "$line_no" "$token" "fn $name in ${dir#"$root"/}"
        elif [ -n "$dir" ]; then
          report "$line_no" "$token" "no \`fn ${name}\` in ${dir#"$root"/}"
        else
          unattributed_ref "$line_no" "$token" "no crate \`$crate\` to look in"
        fi
        continue
      fi

      # `crate::module` — a module reference, and the file the rows after it mean.
      if [[ $token =~ ^([a-z0-9_]+)::([a-z0-9_]+)$ ]]; then
        crate=${BASH_REMATCH[1]}
        module=${BASH_REMATCH[2]}
        file=$(pick_candidate "$crate" "$module" "")
        if [ -n "$file" ]; then
          checked=$((checked + 1))
          cell_file=$file
          row_file=$file
          note "$line_no" "$token" "${file#"$root"/}"
        fi
        continue
      fi

      # A file, with or without lines: `crates/x/src/y.rs:31,275-307,506`.
      if [[ $token =~ ^([A-Za-z0-9_./-]+\.(rs|toml|md|sh))(:([0-9][0-9,-]*))?$ ]]; then
        part=${BASH_REMATCH[1]}
        spec=${BASH_REMATCH[4]:-}
        checked=$((checked + 1))
        file=$(pick_candidate "" "$part" "")
        if [ -z "$file" ]; then
          report "$line_no" "$token" "no such file: $part"
          continue
        fi
        cell_file=$file
        row_file=$file
        if [ -n "$spec" ]; then
          reason=$(check_lines "$file" "$spec")
          if [ -n "$reason" ]; then
            report "$line_no" "$token" "$reason"
            continue
          fi
          next=${spans[$index]:-}
          next=${next#\`}
          next=${next%\`}
          if promised_name "$next"; then
            short=$(short_name "$next")
            on_line=0
            while IFS= read -r n; do
              if sed -n "${n}p" "$file" | grep -qF "$short"; then
                on_line=1
                break
              fi
            done < <(each_line "$spec")
            if [ "$on_line" = 0 ]; then
              elsewhere=$(name_line "$file" "$short")
              if [ -n "$elsewhere" ]; then
                report "$line_no" "$token" "line $spec does not hold \`$next\` (its first line in ${file#"$root"/} is $elsewhere)"
              else
                report "$line_no" "$token" "no \`$next\` in ${file#"$root"/}"
              fi
              continue
            fi
          fi
        fi
        note "$line_no" "$token" "${file#"$root"/}"
        continue
      fi

      # `:117` — another line of the file this cell, row or section named.
      if [[ $token =~ ^:([0-9][0-9,-]*)$ ]]; then
        spec=${BASH_REMATCH[1]}
        file=$cell_file
        [ -z "$file" ] && file=$row_file
        [ -z "$file" ] && file=$scope_file
        checked=$((checked + 1))
        if [ -z "$file" ]; then
          unattributed_ref "$line_no" "$token" "a line with no file named near it (cell, row, section)"
          continue
        fi
        reason=$(check_lines "$file" "$spec")
        if [ -n "$reason" ]; then
          report "$line_no" "$token" "$reason"
          continue
        fi
        note "$line_no" "$token" "${file#"$root"/}:$spec"
        continue
      fi

      # A directory a row names: `crates/settings`, `docs/screens`.
      if [[ $token =~ ^(crates|docs|scripts)/[A-Za-z0-9_./-]+$ ]]; then
        checked=$((checked + 1))
        if [ -e "$root/$token" ]; then
          note "$line_no" "$token" "$token"
        else
          report "$line_no" "$token" "no such path: $token"
        fi
      fi
    done
  done < <(tr '|' '\n' <<<"$line")
done <"$audit"

printf '\n%s citation(s) read: %s broken, %s unattributed\n' \
  "$checked" "$broken" "$unattributed"
[ "$broken" -eq 0 ]
