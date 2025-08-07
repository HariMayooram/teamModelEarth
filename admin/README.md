Always use a virtualenv to protect your OS.
We use one virtualenv for an http server on port 8887, and another for Claude Code CLL.

Fork and clone a [webroot containing the team repo](https://github.com/modelearth/codechat/).  

In your webroot, run a web server within a virtual environment. 

On Macs:

	python3 -m venv env
	source env/bin/activate
	python -m http.server 8887

<br>On Windows:

	python -m venv env
	env\Scripts\activate
	python -m http.server 8887
