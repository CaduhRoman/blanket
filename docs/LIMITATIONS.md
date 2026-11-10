# Limitações reais do Servo 0.8 no Blanket

O Blanket **não desliga** verificações de segurança do motor para ganhar desempenho. Onde o Servo não oferece API, a alternativa está documentada aqui.

## Motor

- **Não é Chromium.** Sites que dependem de APIs ainda incompletas no Servo (algumas formas de WebRTC, codecs, APIs novas de CSS/JS) podem falhar. Isso é uma limitação do motor, não do chrome.
- **Um processo de conteúdo.** O Servo 0.8 não expõe um processo por aba ao embedder. Isolamento entre origens continua a ser o do motor (same-origin, CSP, mixed content, certificados). Isolamento *de processo* entre abas não existe nesta versão.
- **Histórico de sessão do motor.** `WebView` não restaura a lista back/forward após destruir a view. Abas suspensas guardam os URLs no Blanket e voltam a `load`; o botão Voltar usa essa lista + `can_go_back()` da view viva.
- **Downloads.** Não há API de download. O Blanket intercepta navegações de documento principal para extensões típicas de ficheiro, e corre o transfer em `blanket-net` (rustls, cookies do motor).
- **Áudio/vídeo.** Sem a feature `media` (GStreamer), tipos de média são tratados como download. Dummy media do Servo não reproduz.
- **Certificados.** A validação TLS é a do Servo. O Blanket não substitui o verificador nem adiciona um “clique para ignorar certificado inválido” genérico.

## Memória e CPU

- O RSS reportado é do **processo inteiro**. O Servo não atribui bytes por aba de forma fiável ao embedder; o Memory Manager usa um custo estimado e pressão do sistema.
- Suspender uma aba destrói o `WebView` e liberta os recursos do motor dessa aba. Formulários não gravados, média a tocar e abas fixadas **não** são suspensos automaticamente. Há um prompt se a página tiver `beforeunload`.
- Timers de páginas em segundo plano continuam sujeitos à política do Servo; o Blanket apenas esconde views inativas (o pintor salta-as) e reduz o trabalho do chrome.

## Cache

O cache HTTP é o do Servo. O Blanket decide *quando* o largar (pressão crítica, “limpar dados”) e orçamenta caches próprios (favicons, miniaturas). Não reimplementa `Cache-Control`.

## New tab e esquemas internos

`blanket:` só serve `newtab` e o JPEG da seda. Histórico, favoritos e definições são painéis nativos, para nenhuma página (interna ou não) ler esse estado. Conteúdo web não pode navegar para `blanket:settings` nem para `file:`.

## Extensões

Não há WebExtensions. Não está planeado emular a API de extensões do Chrome em cima do Servo.
