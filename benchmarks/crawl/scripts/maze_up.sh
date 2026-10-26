#!/bin/bash
cd ~/pagesift/benchmarks/common/vendor/security-crawl-maze
[ -d venv ] || python3 -m venv --without-pip venv
if [ ! -x venv/bin/flask ]; then
  venv/bin/python /tmp/get-pip.py >/dev/null 2>&1
  venv/bin/python -m pip install -q "flask" jinja2 2>&1 | tail -3
fi
PORT=8099 nohup venv/bin/python app.py > /tmp/maze.log 2>&1 &
sleep 4
curl -sS -o /dev/null -w "index=%{http_code}\n" http://127.0.0.1:8099/
curl -sS http://127.0.0.1:8099/robots.txt
