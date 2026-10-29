/* SPDX-License-Identifier: Apache-2.0
 * Zeus's read-only Everything SDK2 process broker. No INI/profile/registry/filesystem-index
 * access, helper execution, GUI controls, history mutation or network transport is exposed
 * through this facade. The vendored SDK also contains APIs the facade never calls.
 * The parent owns the hard deadline and validates every returned path against current roots.
 */
#define UNICODE
#define _UNICODE
#define EVERYTHINGUSERAPI __declspec(dllexport)
#include <windows.h>
#include <wchar.h>
#include <stdlib.h>
#include <stdio.h>
#include "sdk/include/Everything.h"
#include "sdk/ipc/Everything_IPC.h"

static wchar_t zeus_window_class[160];
/* The legacy SDK has no instance selector. Override only its window lookup class; the
 * original SDK source and its MIT notices are retained unchanged below. */
#undef EVERYTHING_IPC_WNDCLASS
#define EVERYTHING_IPC_WNDCLASS zeus_window_class
#include "sdk/src/Everything.c"

static DWORD zeus_output_bytes;
static int write_bytes(HANDLE output,const char *bytes,DWORD count) {
    DWORD done=0;
    if(count>8*1024*1024-zeus_output_bytes)return 0;
    zeus_output_bytes+=count;
    while(count) {DWORD n=0;if(!WriteFile(output,bytes+done,count,&n,NULL)||!n)return 0;done+=n;count-=n;}
    return 1;
}
static int write_string(HANDLE output,const wchar_t *text,DWORD length) {
    int bytes=WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,text,(int)length,NULL,0,NULL,NULL);
    char *utf8;int ok=1;
    if(bytes<=0 || bytes>131072)return 0;
    utf8=(char*)malloc((size_t)bytes);if(!utf8)return 0;
    if(WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,text,(int)length,utf8,bytes,NULL,NULL)!=bytes){free(utf8);return 0;}
    if(!write_bytes(output,"\"",1)){free(utf8);return 0;}
    int start=0;
    for(int i=0;i<bytes&&ok;i++) {
        unsigned char c=(unsigned char)utf8[i];
        if(c=='"'||c=='\\'||c<32) {
            if(i>start)ok=write_bytes(output,utf8+start,(DWORD)(i-start));
            if(c=='"'||c=='\\') {char escaped[2]={'\\',(char)c};ok=ok&&write_bytes(output,escaped,2);}
            else {char escaped[7];snprintf(escaped,sizeof(escaped),"\\u%04x",c);ok=ok&&write_bytes(output,escaped,6);}
            start=i+1;
        }
    }
    if(start<bytes)ok=ok&&write_bytes(output,utf8+start,(DWORD)(bytes-start));
    free(utf8);return ok&&write_bytes(output,"\"",1);
}
static int decimal(const wchar_t *text,DWORD *value) {
    unsigned long n=0;if(!*text)return 0;
    while(*text){if(*text<L'0'||*text>L'9')return 0;n=n*10+(unsigned)(*text++-L'0');if(n>1000000)return 0;}
    *value=(DWORD)n;return 1;
}
static int literal_pattern(const wchar_t *text) {
    if(!wcscmp(text,L"(?:)"))return 1;
    while(*text) {
        unsigned value=0,digits=0;
        if(text[0]!=L'\\'||text[1]!=L'x'||text[2]!=L'{')return 0;text+=3;
        while(*text && *text!=L'}'){unsigned digit;if(*text>=L'0'&&*text<=L'9')digit=(unsigned)(*text-L'0');else if(*text>=L'a'&&*text<=L'f')digit=10+(unsigned)(*text-L'a');else return 0;value=value*16+digit;digits++;if(digits>6)return 0;text++;}
        if(*text!=L'}'||!digits||value>0x10ffff||(value>=0xd800&&value<=0xdfff)||value<32)return 0;text++;
    }
    return 1;
}
int wmain(int argc,wchar_t **argv) {
    DWORD offset,sort;HANDLE output;wchar_t root[32768],query[40000],module[32768],result[32768];
    const DWORD sorts[8]={EVERYTHING_SORT_NAME_ASCENDING,EVERYTHING_SORT_NAME_DESCENDING,EVERYTHING_SORT_PATH_ASCENDING,EVERYTHING_SORT_PATH_DESCENDING,EVERYTHING_SORT_SIZE_ASCENDING,EVERYTHING_SORT_SIZE_DESCENDING,EVERYTHING_SORT_DATE_MODIFIED_ASCENDING,EVERYTHING_SORT_DATE_MODIFIED_DESCENDING};
    if(argc==2&&!wcscmp(argv[1],L"--protocol-version")){fputs("zeus-everything-sdk2-v1\n",stdout);return 0;}
    if(argc!=8||wcscmp(argv[1],L"--zeus-everything-sdk2-v1")||wcslen(argv[2])>32000||wcslen(argv[3])>8192||wcslen(argv[4])>64||!decimal(argv[5],&offset)||!decimal(argv[6],&sort)||sort>7||!literal_pattern(argv[3]))return 6;
    for(const wchar_t *p=argv[4];*p;p++)if(!((*p>=L'a'&&*p<=L'z')||(*p>=L'A'&&*p<=L'Z')||(*p>=L'0'&&*p<=L'9')||*p==L'.'||*p==L'_'||*p==L'-'))return 6;
    DWORD root_length=GetFullPathNameW(argv[2],32768,root,NULL);
    if(!root_length||root_length>=32768||wcslen(root)>32000||wcspbrk(root,L"\"<>|\r\n"))return 6;
    DWORD module_length=GetModuleFileNameW(NULL,module,32768);if(!module_length||module_length>=32768)return 6;
    wchar_t *separator=wcsrchr(module,L'\\');if(!separator)return 6;separator[1]=0;
    if(wcslen(module)+12>=32768)return 6;wcscat(module,L"results.json");
    if(CompareStringOrdinal(module,-1,argv[7],-1,TRUE)!=CSTR_EQUAL)return 6;
    if(*argv[4])swprintf(zeus_window_class,160,L"EVERYTHING_TASKBAR_NOTIFICATION_(%ls)",argv[4]);else wcscpy(zeus_window_class,L"EVERYTHING_TASKBAR_NOTIFICATION");
    if(!FindWindowW(zeus_window_class,NULL))return 8;
    size_t length=wcslen(root);int separator_needed=length&&root[length-1]!=L'\\'&&root[length-1]!=L'/';
    if(swprintf(query,40000,L"\"%ls%ls\" regex:%ls",root,separator_needed?L"\\":L"",argv[3])<0)return 6;
    Everything_SetSearchW(query);Everything_SetMatchPath(FALSE);Everything_SetMatchCase(FALSE);Everything_SetMatchWholeWord(FALSE);Everything_SetRegex(FALSE);
    Everything_SetRequestFlags(EVERYTHING_REQUEST_FULL_PATH_AND_FILE_NAME);Everything_SetOffset(offset);Everything_SetMax(501);Everything_SetSort(sorts[sort]);
    /* SDK2 waits can block inside SendMessage. This process has no child processes; Zeus kills
       and joins it after ten seconds. Never host this blocking call on the app's own thread. */
    if(!Everything_QueryW(TRUE))return 7;
    DWORD count=Everything_GetNumResults();if(count>501){Everything_CleanUp();return 7;}
    output=CreateFileW(module,GENERIC_WRITE,0,NULL,CREATE_NEW,FILE_ATTRIBUTE_NORMAL,NULL);if(output==INVALID_HANDLE_VALUE){Everything_CleanUp();return 5;}
    int ok=write_bytes(output,"[",1);
    for(DWORD i=0;i<count&&ok;i++) {
        DWORD chars=Everything_GetResultFullPathNameW(i,result,32768);
        if(!chars||chars>=32768){ok=0;break;}
        if(i)ok=write_bytes(output,",",1);
        ok=ok&&write_bytes(output,"{\"filename\":",12)&&write_string(output,result,chars)&&write_bytes(output,"}",1);
    }
    ok=ok&&write_bytes(output,"]\n",2);CloseHandle(output);Everything_CleanUp();return ok?0:7;
}
