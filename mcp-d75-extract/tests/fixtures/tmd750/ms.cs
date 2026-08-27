public class ms
{
	private int r;

	private List<StatusTextData> x;

	private List<ObjectData> ay;

	private List<UserPhraseData> bn;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			r = value;
		}
	}

	public byte QsyLimit
	{
		get { return 0; }
	}

	public bool QsyInStatus
	{
		get { return false; }
	}

	public List<StatusTextData> StatusTextList
	{
		get
		{
			return x;
		}
	}

	public List<ObjectData> ObjectList
	{
		get
		{
			return ay;
		}
	}

	public List<UserPhraseData> UserPhraseList
	{
		get
		{
			return bn;
		}
	}

	public void ai()
	{
		int num5 = 0;
		int num6 = 0;
		int num4 = 0;
		while (num5 < 5)
		{
			x.Add(new StatusTextData(num5));
			x[num5].OffsetProgrammableMemoryAddress = r;
			num5++;
		}
		while (num6 < 3)
		{
			ay.Add(new ObjectData(null));
			ay[num6].OffsetProgrammableMemoryAddress = r;
			num6++;
		}
		while (num4 < 20)
		{
			bn.Add(new UserPhraseData());
			bn[num4].OffsetProgrammableMemoryAddress = r;
			num4++;
		}
	}

	public void a6(n7 A_0)
	{
		int num4 = 0;
		int num6 = 0;
		int num5 = 0;
		A_0.a(QsyInStatus, 329825 + r);
		A_0.a(QsyLimit, 329828 + r);
		while (num4 < 3)
		{
			ObjectList[num4].a3(A_0, num4);
			num4++;
		}
		while (num6 < 5)
		{
			StatusTextList[num6].b(A_0, num6);
			num6++;
		}
		while (num5 < 20)
		{
			UserPhraseList[num5].b(A_0, num5);
			num5++;
		}
	}

	public void a7(n7 A_0)
	{
		QsyLimit = A_0.a(329828 + r);
	}
}
