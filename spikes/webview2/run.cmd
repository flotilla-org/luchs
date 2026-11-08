@echo off
rem Runs the spike from this directory; frames and the WebView2 user data folder land here.
rem   run.cmd           the full run (Q1-Q7)
rem   run.cmd followup  the follow-up run (focus, CDP input, cursor, visibility, MoveFocus)
cd /d "%~dp0"
"%~dp0target\release\wv2spike.exe" %1
