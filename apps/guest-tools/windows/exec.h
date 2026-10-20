/* Guest-only command execution. One process tree per control connection. */
#define EXEC_OUTPUT 2048
typedef struct {
    CRITICAL_SECTION lock;
    HANDLE process, group, worker, output, error;
    uint64_t id;
    DWORD started, timeout, code;
    int active, timed_out, cancelled, truncated;
    char stdout_data[EXEC_OUTPUT+1], stderr_data[EXEC_OUTPUT+1];
    size_t stdout_size, stderr_size;
} GuestExec;
static GuestExec execution;

static void exec_drain(HANDLE pipe,char *data,size_t *size){
    char buffer[4096];DWORD available,n;
    /* A busy producer must not postpone timeout checks indefinitely. */
    unsigned rounds=0;
    while(rounds++<16 && PeekNamedPipe(pipe,NULL,0,NULL,&available,NULL) && available){
        size_t keep;
        if(!ReadFile(pipe,buffer,available>sizeof(buffer)?sizeof(buffer):available,&n,NULL)||!n)break;
        keep=EXEC_OUTPUT-*size;if(keep>n)keep=n;
        memcpy(data+*size,buffer,keep);*size+=keep;
        if(keep<n)execution.truncated=1;
    }
    data[*size]=0;
}
static DWORD WINAPI exec_worker(void *unused){
    (void)unused;
    for(;;){
        int done;
        EnterCriticalSection(&execution.lock);
        exec_drain(execution.output,execution.stdout_data,&execution.stdout_size);
        exec_drain(execution.error,execution.stderr_data,&execution.stderr_size);
        done=WaitForSingleObject(execution.process,0)==WAIT_OBJECT_0;
        if(!done && (execution.cancelled || GetTickCount()-execution.started>=execution.timeout)){
            if(!execution.cancelled)execution.timed_out=1;
            TerminateJobObject(execution.group,1);
        }
        if(done){
            GetExitCodeProcess(execution.process,&execution.code);
            TerminateJobObject(execution.group,1);
            exec_drain(execution.output,execution.stdout_data,&execution.stdout_size);
            exec_drain(execution.error,execution.stderr_data,&execution.stderr_size);
            execution.active=0;
        }
        LeaveCriticalSection(&execution.lock);
        if(done)break;
        Sleep(10);
    }
    return 0;
}
static void exec_clear(void){
    EnterCriticalSection(&execution.lock);
    if(execution.active){execution.cancelled=1;TerminateJobObject(execution.group,1);}
    LeaveCriticalSection(&execution.lock);
    if(execution.worker){WaitForSingleObject(execution.worker,INFINITE);CloseHandle(execution.worker);execution.worker=NULL;}
    if(execution.process)CloseHandle(execution.process);
    if(execution.group)CloseHandle(execution.group);
    if(execution.output)CloseHandle(execution.output);
    if(execution.error)CloseHandle(execution.error);
    execution.process=execution.group=execution.output=execution.error=NULL;
    execution.id=0;
}
/* Console programs generally emit OEM bytes. Convert to bounded UTF-8 and
   represent embedded NUL bytes as replacement characters. */
static void exec_quote(JOut *out,const char *data,size_t length){
    WCHAR wide_data[EXEC_OUTPUT+1];char utf8[EXEC_OUTPUT*3+1];int n,m,i;
    if(!length){jquote(out,"");return;}
    n=MultiByteToWideChar(CP_OEMCP,0,data,(int)length,wide_data,EXEC_OUTPUT);
    for(i=0;i<n;++i)if(!wide_data[i])wide_data[i]=0xfffd;
    m=WideCharToMultiByte(CP_UTF8,0,wide_data,n,utf8,sizeof(utf8)-1,NULL,NULL);
    utf8[m]=0;jquote(out,utf8);
}
static void exec_reply(JOut *out){
    EnterCriticalSection(&execution.lock);
    jtext(out,"{\"result\":\"exec\",\"job\":");juint(out,execution.id);
    jtext(out,",\"running\":");jtext(out,execution.active?"true":"false");
    jtext(out,",\"exit_code\":");if(execution.active)jtext(out,"null");else jint(out,(int32_t)execution.code);
    jtext(out,",\"stdout\":");exec_quote(out,execution.stdout_data,execution.stdout_size);
    jtext(out,",\"stderr\":");exec_quote(out,execution.stderr_data,execution.stderr_size);
    jtext(out,",\"truncated\":");jtext(out,execution.truncated?"true":"false");
    jtext(out,",\"timed_out\":");jtext(out,!execution.active&&execution.timed_out?"true":"false");
    jtext(out,",\"cancelled\":");jtext(out,!execution.active&&execution.cancelled?"true":"false");jtext(out,"}");
    LeaveCriticalSection(&execution.lock);
}
static void guest_exec(JOut *out,const JDoc *doc,int command,uint64_t id,int start,int cancel){
    HANDLE output_write=NULL,error_write=NULL,input=NULL;
    if(start){
        char text[8193];WCHAR line[8256],shell[MAX_PATH];
        uint64_t timeout=jnumber(doc,jget(doc,command,"timeout_ms"),0);
        SECURITY_ATTRIBUTES sa={sizeof(sa),NULL,TRUE};
        STARTUPINFOW si;PROCESS_INFORMATION pi;JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits;
        EnterCriticalSection(&execution.lock);
        if(execution.active){LeaveCriticalSection(&execution.lock);error_reply(out,"execution_busy");return;}
        LeaveCriticalSection(&execution.lock);
        if(!id||!timeout||timeout>120000||!jstring(doc,jget(doc,command,"command"),text,sizeof(text))||!text[0]){error_reply(out,"invalid_execution");return;}
        exec_clear();
        memset(&si,0,sizeof(si));memset(&pi,0,sizeof(pi));memset(&limits,0,sizeof(limits));
        execution.group=CreateJobObjectW(NULL,NULL);
        limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if(!execution.group||!SetInformationJobObject(execution.group,JobObjectExtendedLimitInformation,&limits,sizeof(limits)))goto failed;
        if(!CreatePipe(&execution.output,&output_write,&sa,0)||!CreatePipe(&execution.error,&error_write,&sa,0))goto failed;
        if(!SetHandleInformation(execution.output,HANDLE_FLAG_INHERIT,0)||!SetHandleInformation(execution.error,HANDLE_FLAG_INHERIT,0))goto failed;
        input=CreateFileW(L"NUL",GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE,&sa,OPEN_EXISTING,0,NULL);
        if(input==INVALID_HANDLE_VALUE){input=NULL;goto failed;}
        if(!GetSystemDirectoryW(shell,MAX_PATH)||!wappend(shell,MAX_PATH,L"\\cmd.exe"))goto failed;
        memcpy(line,L"cmd.exe /d /s /c \"",18*sizeof(WCHAR));
        if(!wide(text,line+18,8193)||!wappend(line,8256,L"\""))goto failed;
        si.cb=sizeof(si);si.dwFlags=STARTF_USESTDHANDLES;si.hStdInput=input;si.hStdOutput=output_write;si.hStdError=error_write;
        if(!CreateProcessW(shell,line,NULL,NULL,TRUE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&si,&pi))goto failed;
        if(!AssignProcessToJobObject(execution.group,pi.hProcess)){TerminateProcess(pi.hProcess,1);CloseHandle(pi.hProcess);CloseHandle(pi.hThread);goto failed;}
        execution.process=pi.hProcess;execution.id=id;execution.timeout=(DWORD)timeout;execution.started=GetTickCount();
        execution.stdout_size=execution.stderr_size=0;execution.timed_out=execution.cancelled=execution.truncated=0;execution.active=1;
        ResumeThread(pi.hThread);CloseHandle(pi.hThread);
        CloseHandle(output_write);output_write=NULL;CloseHandle(error_write);error_write=NULL;CloseHandle(input);input=NULL;
        execution.worker=CreateThread(NULL,0,exec_worker,NULL,0,NULL);
        if(!execution.worker){TerminateJobObject(execution.group,1);execution.active=0;goto failed;}
    }else{
        uint64_t job=jnumber(doc,jget(doc,command,"job"),0);
        if(!job||job!=execution.id){error_reply(out,"unknown_execution");return;}
        EnterCriticalSection(&execution.lock);
        if(cancel&&execution.active){execution.cancelled=1;TerminateJobObject(execution.group,1);}
        LeaveCriticalSection(&execution.lock);
    }
    exec_reply(out);return;
failed:
    if(output_write)CloseHandle(output_write);if(error_write)CloseHandle(error_write);if(input)CloseHandle(input);
    exec_clear();error_reply(out,"execution_failed");
}
