# Blanket

Navegador desktop em Rust, com **Servo** como motor de renderização. O chrome é nativo (egui + winit); as páginas são HTML, CSS e JavaScript reais, pintados pelo Servo.

Não é um shell Chromium, CEF, WebKit ou Electron.

## O que já funciona

- Navegação HTTP/HTTPS em sites reais, com o motor Servo 0.8.
- Várias abas, sessão, histórico com frecency, favoritos, downloads.
- Barra de endereço inteligente, atalhos, painéis nativos (histórico, favoritos, downloads, definições, desempenho).
- Suspensão inteligente de abas inativas, orçamento adaptativo de abas vivas, agendamento de tarefas em segundo plano.
- Cache HTTP do próprio Servo (respeita `Cache-Control`, validação, `Vary`); caches da UI com orçamento em bytes.
- Página inicial `blanket:newtab` servida em memória, sem APIs privilegiadas.

## Requisitos

- Windows 10/11 x64
- Rust 1.88 ou mais recente (`rustup`)
- LLVM/clang no `PATH` (o Servo usa `bindgen` / mozangle)
- Python 3 (algumas crates nativas do Servo)
- ~20 GB livres para a primeira compilação

Detalhes em [`docs/BUILDING.md`](docs/BUILDING.md). Limitações reais do motor em [`docs/LIMITATIONS.md`](docs/LIMITATIONS.md).

## Compilar e correr

```powershell
# Lógica, políticas e testes — sem Servo, leva segundos:
cargo test -p blanket-core -p blanket-net

# Navegador (primeira vez: dezenas de minutos):
cargo run -p blanket --release
```

Abra um URL na linha de comando:

```powershell
cargo run -p blanket --release -- https://servo.org
```

O perfil fica em `%APPDATA%\Blanket` (configuração, histórico, cookies do motor, logs). Use `--profile-dir` para um perfil temporário.

## Benchmarks

```powershell
cargo run -p blanket-bench --release -- --tabs 1,5,10 --url https://example.com/ --output bench-results/run.json
```

O harness mede Blanket, e Chrome/Firefox se estiverem instalados, nas mesmas URLs e contagens de abas. Os números são do *seu* computador; o relatório JSON não afirma superioridade. Ver [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

## Arquitetura

| Crate | Responsabilidade |
| --- | --- |
| `blanket-core` | Decisões: abas, memória, agendador, cache LRU, histórico, sessão, segurança. Sem Servo. |
| `blanket-net` | Downloads e DNS prefetch (Servo não expõe gestor de downloads ao embedder). |
| `blanket` | Janela, chrome, integração Servo (`WebView`, `WebViewDelegate`, `ServoBuilder`). |
| `blanket-bench` | Medições reproduzíveis. |

A UI nunca fala com o motor: recebe um `UiModel` e devolve `UiCommand`. O conteúdo web é o framebuffer offscreen do Servo, copiado para a área central.

`third_party/` guarda cópias de crates do Servo com correções que a API de embedder não alcança (por exemplo, o limite de cabeçalhos HTTP/2 que impedia o YouTube de carregar). Ver [`third_party/README.md`](third_party/README.md).

## Identidade

Chrome escuro, controlos em vidro (material translúcido no espírito do iOS), um único verde `#3dd68c`, wordmark **blanket**. As abas ocupam a barra de título, com os controlos da janela no canto superior direito, como no Chrome. Transições suaves em abas, botões, menus e painéis. A new tab usa a ilustração de seda líquida do projeto como fundo de ecrã inteiro.
