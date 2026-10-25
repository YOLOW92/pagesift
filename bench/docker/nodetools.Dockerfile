# D1 的 Node 竞品镜像：@mozilla/readability、defuddle、turndown、node-html-markdown
FROM node:22-slim
ENV NODE_ENV=production
RUN set -eux; \
    mkdir -p /tools && cd /tools && npm init -y >/dev/null 2>&1; \
    for pkg in jsdom @mozilla/readability defuddle turndown node-html-markdown; do \
        echo "=== installing $pkg"; npm install --no-audit --no-fund "$pkg" || echo "INSTALL_FAILED $pkg"; \
    done
RUN node -e "for (const m of ['jsdom','@mozilla/readability','defuddle','turndown','node-html-markdown']) { try { require(m); console.log('OK', m) } catch (e) { console.log('FAIL', m, e.code||e.name) } }"
