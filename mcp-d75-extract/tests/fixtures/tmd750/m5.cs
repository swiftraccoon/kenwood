public class m5
{
	private int j;

	private List<MyDvMessageData> k;

	private List<MyCallsignDvGatewayData> l;

	private List<DvUrlData> at;

	private List<DvUrlData> a1;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			j = value;
		}
	}

	public bool DirectReplyTxRx
	{
		get { return false; }
	}

	public List<MyDvMessageData> MyDvMessageList
	{
		get
		{
			return k;
		}
	}

	public List<MyCallsignDvGatewayData> MyCallsignDvGatewayList
	{
		get
		{
			return l;
		}
	}

	public List<DvUrlData> ReflectorHostsUrlList
	{
		get
		{
			return at;
		}
	}

	public List<DvUrlData> AutoUpdateUrlList
	{
		get
		{
			return a1;
		}
	}

	public void ai()
	{
		int num2 = 0;
		int num7 = 0;
		int num4 = 0;
		int num5 = 0;
		while (num2 < 5)
		{
			k.Add(new MyDvMessageData());
			k[num2].OffsetProgrammableMemoryAddress = j;
			num2++;
		}
		while (num7 < 6)
		{
			l.Add(new MyCallsignDvGatewayData());
			l[num7].OffsetProgrammableMemoryAddress = j;
			num7++;
		}
		while (num4 < 1)
		{
			at.Add(new DvUrlData());
			at[num4].OffsetProgrammableMemoryAddress = j;
			at[num4].StartAddress = 334081;
			num4++;
		}
		while (num5 < 1)
		{
			a1.Add(new DvUrlData());
			a1[num5].OffsetProgrammableMemoryAddress = j;
			a1[num5].StartAddress = 335106;
			num5++;
		}
	}

	public void a6(n7 A_0)
	{
		int num4 = 0;
		int num2 = 0;
		A_0.a(DirectReplyTxRx, 331520 + j);
		while (num4 < 6)
		{
			MyCallsignDvGatewayList[num4].b(A_0, num4);
			num4++;
		}
		while (num2 < 5)
		{
			MyDvMessageList[num2].b(A_0, num2);
			num2++;
		}
		ReflectorHostsUrlList[0].b(A_0, 0);
		AutoUpdateUrlList[0].b(A_0, 0);
	}

	public void a7(n7 A_0)
	{
		DirectReplyTxRx = A_0.a(331520 + j) != 0;
	}
}
