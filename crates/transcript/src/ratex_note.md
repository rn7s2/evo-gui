## Math in this client

The client that renders this session draws TeX math as pictures. Write formulas
the way a LaTeX reader expects; no markup of your own and no note about the
rendering.

Delimiters: `$x^2$` inline, `$$…$$` display (on one line or on their own lines).
`\(x\)` and `\[x\]` are read the same way, and a `\[x\]` alone on its own lines
stays a block. A `$` with a space against it is prose, so `it costs $5 and $10`
stays a sentence — but a tight pair is math wherever it appears, and `$5-$10` is
read as the formula `5-` and then the text `10`. Keep a paragraph to one money
amount, or name the currency (`USD 5`).

Drawn: fractions, roots, sub- and superscripts, sums and integrals with limits,
`\left…\right`, matrices (`matrix` and its `pmatrix`/`bmatrix`/`vmatrix` forms),
the multi-line environments (`align`, `aligned`, `split`, `cases`, `array`,
`gathered`, `equation`, `smallmatrix`), `\text{…}`, `\mathbb`-style faces,
spacing and colour commands, macros (`\def`, `\newcommand`), and `\ce{…}` for
chemistry. A picture is self-contained: the same on every machine, with nothing
fetched to make it.

Shown as the source you wrote, delimiters and all, instead of a picture: a
formula that does not parse (`\frac{a}{`), one naming a command the engine does
not have (`\usepackage`, and packages generally), one holding a character typed
literally outside ASCII — `α`, `×`, `→`, a typographic quote; write `\alpha`,
`\times`, `\to` — one longer than 8 KB of source, and one too wide for its place
(an inline formula has a line's width, so keep a long one in its own `$$…$$`
paragraph, which scrolls sideways when it is wider than the pane).

Code — fenced, inline, or indented — is never read as math, so a dollar sign in a
code sample needs no escaping, and an HTML block is drawn as the markup it is. A
copy of a formula gives back the LaTeX you wrote.
